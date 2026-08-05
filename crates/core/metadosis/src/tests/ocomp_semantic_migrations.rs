use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_ocomp_protocol::{
    receipts::ActivationOutcome,
    state::OcompJobStatus,
    vote::{OcompVoteAccountabilityV1, ResultVotePrefixV1},
};
use outbe_primitives::{
    addresses::{INTEX_ADDRESS, NOD_ADDRESS, PROMIS_LIMIT_ADDRESS, TRIBUTE_ADDRESS},
    error::PrecompileError,
};
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::{MetadosisMutationPurposeTag, StorageHandle},
};

use crate::{
    fixture_kernel::{
        ActivationFixture, ActivationMetadata, ActivationReceiptFault, TEST_LOGICAL_TIME,
    },
    ocomp::schema::poc_schema_limits,
    precompile::IMetadosis,
    schema::MetadosisContract,
};

fn close_completed_response_window(
    fixture: &mut ActivationFixture,
    deadline_height: u64,
) -> outbe_primitives::error::Result<()> {
    fixture.provider.set_block_number(deadline_height);
    fixture.provider.set_timestamp(U256::from(1_011));
    fixture
        .provider
        .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(deadline_height, 1_011, 1),
            storage,
        );
        crate::commands::run_ocomp_lifecycle_begin(&ctx)
    })
}

// OCOMP-TEST-ID: OCM-APL-002
// OCOMP-TEST-ID: OCM-E2E-006
#[test]
fn q_forming_faults_restore_all_state_and_exact_retry_matches_clean_execution() {
    for (owner, address) in [
        ("Nod", NOD_ADDRESS),
        ("Contributor", INTEX_ADDRESS),
        ("Tribute", TRIBUTE_ADDRESS),
        ("CarryOver", PROMIS_LIMIT_ADDRESS),
    ] {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        let before = fixture.rollback_snapshot();
        fixture.provider.fail_mutation_at_address(address);

        let error = fixture
            .apply()
            .expect_err("q-forming owner fault must abort the whole command");
        assert!(
            matches!(
                error,
                PrecompileError::Storage(_)
                    | PrecompileError::Fatal(_)
                    | PrecompileError::Revert(_)
                    | PrecompileError::RevertBytes(_)
            ),
            "{owner} failure returned the wrong error class"
        );
        fixture.provider.clear_mutation_failure();
        assert_eq!(fixture.rollback_snapshot(), before);
        assert_eq!(fixture.apply().unwrap(), Bytes::new());
        assert_eq!(fixture.terminal_outcome(), ActivationOutcome::Applied);
    }

    for fault in [
        ActivationReceiptFault::Nod,
        ActivationReceiptFault::Contributor,
        ActivationReceiptFault::Tribute,
        ActivationReceiptFault::CarryOver,
        ActivationReceiptFault::RequestSplit,
    ] {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        let before = fixture.rollback_snapshot();

        assert!(matches!(
            fixture.apply_with_receipt_fault(fault),
            Err(PrecompileError::RevertBytes(_))
        ));
        assert_eq!(fixture.rollback_snapshot(), before);
        assert_eq!(fixture.apply().unwrap(), Bytes::new());
        assert_eq!(fixture.terminal_outcome(), ActivationOutcome::Applied);
    }

    let mut control = ActivationFixture::new(20, 1_010, true);
    control.provider.fail_after_mutation_at(usize::MAX);
    assert_eq!(control.apply().unwrap(), Bytes::new());
    let mutation_count = control.provider.clear_mutation_failure();
    assert!(
        mutation_count >= 10,
        "q-forming must cross vote, quorum, owner, terminal, index, receipt and event boundaries"
    );
    let clean_after = control.rollback_snapshot();

    for operation in 0..mutation_count {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        let before = fixture.rollback_snapshot();
        fixture.provider.fail_after_mutation_at(operation);

        assert!(matches!(fixture.apply(), Err(PrecompileError::Storage(_))));
        assert_eq!(
            fixture.provider.clear_mutation_failure(),
            operation + 1,
            "q-forming fault must occur at the selected persistent mutation"
        );
        assert_eq!(
            fixture.rollback_snapshot(),
            before,
            "q-forming mutation fault {operation} leaked state, events, or CE work"
        );

        assert_eq!(fixture.apply().unwrap(), Bytes::new());
        assert_eq!(
            fixture.rollback_snapshot(),
            clean_after,
            "q-forming retry after mutation fault {operation} diverged from clean execution"
        );
        assert_eq!(fixture.terminal_outcome(), ActivationOutcome::Applied);
    }
}

#[test]
fn conflict_q_forming_rolls_back_every_mutation_and_retries_exactly() {
    let mut control = ActivationFixture::new(20, 1_010, false);
    control.provider.fail_after_mutation_at(usize::MAX);
    assert_eq!(control.apply().unwrap(), Bytes::new());
    let mutation_count = control.provider.clear_mutation_failure();
    assert!(
        mutation_count >= 7,
        "conflict q-forming must persist vote, quorum, terminal, scheduler, outer state and events"
    );
    assert_eq!(
        control.terminal_outcome(),
        ActivationOutcome::ConflictResolved
    );
    let clean_after = control.rollback_snapshot();

    for operation in 0..mutation_count {
        let mut fixture = ActivationFixture::new(20, 1_010, false);
        let before = fixture.rollback_snapshot();
        fixture.provider.fail_after_mutation_at(operation);

        assert!(matches!(fixture.apply(), Err(PrecompileError::Storage(_))));
        assert_eq!(fixture.provider.clear_mutation_failure(), operation + 1);
        assert_eq!(fixture.rollback_snapshot(), before);

        assert_eq!(fixture.apply().unwrap(), Bytes::new());
        assert_eq!(fixture.rollback_snapshot(), clean_after);
        assert_eq!(
            fixture.terminal_outcome(),
            ActivationOutcome::ConflictResolved
        );
    }
}

#[test]
fn quorum_preserved_response_close_rolls_back_every_mutation_and_retries_exactly() {
    let prepare = || {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        assert_eq!(fixture.apply().unwrap(), Bytes::new());
        let deadline_height = StorageHandle::enter(&mut fixture.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_job_record(fixture.intent_id, &fixture.limits)
                .unwrap()
                .unwrap()
                .finalized
                .unwrap()
                .deadline_height
        });
        (fixture, deadline_height)
    };

    let (mut control, deadline_height) = prepare();
    control.provider.fail_after_mutation_at(usize::MAX);
    close_completed_response_window(&mut control, deadline_height).unwrap();
    let mutation_count = control.provider.clear_mutation_failure();
    assert!(
        mutation_count >= 2,
        "quorum-preserved close must persist accountability and remove the deadline key"
    );
    let clean_after = control.rollback_snapshot();

    for operation in 0..mutation_count {
        let (mut fixture, deadline_height) = prepare();
        let before = fixture.rollback_snapshot();
        fixture.provider.fail_after_mutation_at(operation);

        assert!(matches!(
            close_completed_response_window(&mut fixture, deadline_height),
            Err(PrecompileError::Storage(_))
        ));
        assert_eq!(fixture.provider.clear_mutation_failure(), operation + 1);
        assert_eq!(fixture.rollback_snapshot(), before);

        close_completed_response_window(&mut fixture, deadline_height).unwrap();
        assert_eq!(fixture.rollback_snapshot(), clean_after);
        assert_eq!(fixture.terminal_outcome(), ActivationOutcome::Applied);
    }
}

// OCOMP-TEST-ID: OCM-TIM-001
#[test]
fn request_pinned_semantics_are_identical_at_different_activation_heights() {
    let mut first = ActivationFixture::new(20, 1_010, true);
    let first_calldata = first.calldata();
    first.apply().expect("first q-forming vote must apply");
    assert_eq!(first.terminal_outcome(), ActivationOutcome::Applied);
    let first_semantics = first.semantic_snapshot();
    let first_metadata = first.activation_metadata();
    let first_state = first.rollback_snapshot();

    let mut second = ActivationFixture::new(40, 2_020, true);
    assert_eq!(
        second.calldata(),
        first_calldata,
        "activation height is not part of the request-pinned computation"
    );
    second.apply().expect("second q-forming vote must apply");
    assert_eq!(second.terminal_outcome(), ActivationOutcome::Applied);
    let second_semantics = second.semantic_snapshot();
    let second_metadata = second.activation_metadata();
    let second_state = second.rollback_snapshot();

    assert_eq!(first_semantics, second_semantics);
    assert_eq!(first_semantics.nod.issued_at, TEST_LOGICAL_TIME);
    assert_eq!(
        first_metadata,
        ActivationMetadata {
            activated_at_height: 20,
            activated_at_time: 1_010,
        }
    );
    assert_eq!(
        second_metadata,
        ActivationMetadata {
            activated_at_height: 40,
            activated_at_time: 2_020,
        }
    );
    assert_ne!(first_state, second_state);
}

fn result_digest(fixture: &ActivationFixture) -> B256 {
    fixture.result.result_digest(&fixture.limits).unwrap()
}

fn submit_vote_result(
    fixture: &mut ActivationFixture,
    vote: &outbe_ocomp_protocol::vote::ResultVoteV1,
    height: u64,
) -> outbe_primitives::error::Result<Bytes> {
    fixture.provider.set_block_number(height);
    fixture.provider.enable_lysis_activation_frame();
    fixture
        .provider
        .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::VerifiedResultVote);
    let vote_bytes = vote.encode_canonical(&fixture.limits).unwrap();
    let calldata = IMetadosis::submitLysisResultCall {
        resultVoteV1: Bytes::from(vote_bytes),
    }
    .abi_encode();
    StorageHandle::enter(&mut fixture.provider, |storage| {
        crate::commands::submit_verified_result_vote(
            storage,
            &fixture.scope,
            &calldata,
            U256::ZERO,
            false,
        )
    })
}

fn submit_vote(fixture: &mut ActivationFixture, validator_index: u8, height: u64) {
    let vote = fixture.signed_result_vote(validator_index);
    assert_eq!(
        submit_vote_result(fixture, &vote, height).unwrap(),
        Bytes::new()
    );
}

fn assert_public_dispatch_uses_pinned_dynamic_membership(
    member_count: u8,
    quorum_threshold: u16,
    quorum_bitmap: Vec<u8>,
) {
    let mut fixture =
        ActivationFixture::new_voting_with_member_count(14, 1_010, true, member_count);
    let job_id = fixture.result.job_id;
    let intent_id = fixture.intent_id;
    let expected_result_digest = result_digest(&fixture);

    for index in 0..member_count {
        submit_vote(&mut fixture, index, 14 + u64::from(index));
    }

    StorageHandle::enter(&mut fixture.provider, |storage| {
        let limits = poc_schema_limits();
        let contract = MetadosisContract::new(storage.clone());
        let record = contract
            .ocomp_job_record(intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::Completed);
        assert_eq!(record.intent.result_member_count, u16::from(member_count));
        assert_eq!(record.intent.result_quorum_threshold, quorum_threshold);
        let quorum = record.finalized.unwrap().quorum.unwrap();
        assert_eq!(quorum.result_digest, expected_result_digest);
        assert_eq!(quorum.signer_bitmap, quorum_bitmap);

        let encoded = crate::precompile::dispatch(
            storage,
            &IMetadosis::getOffchainVoteAccountabilityCall { jobId: job_id }.abi_encode(),
            Address::repeat_byte(0x41),
            U256::ZERO,
        )
        .unwrap();
        let public =
            IMetadosis::getOffchainVoteAccountabilityCall::abi_decode_returns(&encoded).unwrap();
        let accountability =
            OcompVoteAccountabilityV1::decode_canonical(public.as_ref(), &limits).unwrap();
        assert_eq!(accountability.slots.len(), usize::from(member_count));
        assert_eq!(
            accountability.slots.iter().flatten().count(),
            usize::from(member_count)
        );
        assert_eq!(accountability.quorum.unwrap(), quorum);
    });
}

// OCOMP-TEST-ID: OCM-VOT-001
#[test]
fn public_dispatch_uses_pinned_dynamic_membership_and_quorum() {
    assert_public_dispatch_uses_pinned_dynamic_membership(4, 3, vec![0b0000_0111]);
    assert_public_dispatch_uses_pinned_dynamic_membership(5, 4, vec![0b0000_1111]);
}

#[test]
fn historical_vote_participant_resolution_does_not_consult_current_active_status() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let vote = fixture.signed_result_vote(1);
    let prefix = ResultVotePrefixV1 {
        protocol_bundle_hash: vote.protocol_bundle_hash,
        job_id: vote.job_id,
        attempt: vote.attempt,
        result_validator_set_epoch: vote.result_validator_set_epoch,
        result_committee_set_hash: vote.result_committee_set_hash,
        result_ocomp_binding_hash: vote.result_ocomp_binding_hash,
        validator_index: vote.validator_index,
        key_epoch: vote.key_epoch,
    };
    let historical_validator = Address::repeat_byte(0xB1);

    StorageHandle::enter(&mut fixture.provider, |storage| {
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validators
            .val_status
            .write(
                &historical_validator,
                outbe_validatorset::logic::status::INACTIVE,
            )
            .unwrap();

        assert_eq!(
            crate::ocomp::vote::resolve_historical_result_vote_participant(
                storage,
                &prefix,
                &fixture.limits,
            )
            .unwrap(),
            Some(historical_validator)
        );
    });
}

#[test]
fn system_carrier_signer_is_bound_to_the_historical_validator_or_its_delegate() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let vote = fixture.signed_result_vote(1);
    let prefix = ResultVotePrefixV1 {
        protocol_bundle_hash: vote.protocol_bundle_hash,
        job_id: vote.job_id,
        attempt: vote.attempt,
        result_validator_set_epoch: vote.result_validator_set_epoch,
        result_committee_set_hash: vote.result_committee_set_hash,
        result_ocomp_binding_hash: vote.result_ocomp_binding_hash,
        validator_index: vote.validator_index,
        key_epoch: vote.key_epoch,
    };
    let historical_validator = Address::repeat_byte(0xB1);
    let delegate = Address::repeat_byte(0xD1);

    StorageHandle::enter(&mut fixture.provider, |storage| {
        assert_eq!(
            crate::ocomp::vote::resolve_historical_result_vote_carrier_signer(
                storage.clone(),
                &prefix,
                historical_validator,
                &fixture.limits,
            )
            .unwrap(),
            Some(historical_validator)
        );
        assert_eq!(
            crate::ocomp::vote::resolve_historical_result_vote_carrier_signer(
                storage.clone(),
                &prefix,
                delegate,
                &fixture.limits,
            )
            .unwrap(),
            None
        );

        let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validators
            .set_delegate(
                historical_validator,
                outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                delegate,
            )
            .unwrap();

        assert_eq!(
            crate::ocomp::vote::resolve_historical_result_vote_carrier_signer(
                storage.clone(),
                &prefix,
                historical_validator,
                &fixture.limits,
            )
            .unwrap(),
            None,
            "an explicit delegate disables direct carrier submission"
        );
        assert_eq!(
            crate::ocomp::vote::resolve_historical_result_vote_carrier_signer(
                storage,
                &prefix,
                delegate,
                &fixture.limits,
            )
            .unwrap(),
            Some(historical_validator)
        );
    });
}

#[test]
fn vote_binding_mismatch_is_rejected_before_historical_snapshot_lookup() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let mut vote = fixture.signed_result_vote(1);
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        vote.result_validator_set_epoch,
        vote.result_committee_set_hash,
    );
    vote.result_ocomp_binding_hash = B256::repeat_byte(0xEE);

    StorageHandle::enter(&mut fixture.provider, |storage| {
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validators
            .committee_snapshot_exists
            .write(&snapshot_key, false)
            .unwrap();

        let error = MetadosisContract::new(storage)
            .record_ocomp_result_vote(&vote, 14, &fixture.scope, &fixture.limits)
            .unwrap_err();
        assert!(matches!(
            error,
            PrecompileError::Revert(message)
                if message == "OCOMP result vote does not match pinned job binding"
        ));
    });
}

#[test]
fn public_vote_rejects_every_pinned_identity_and_signature_mismatch() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let base = fixture.signed_result_vote(1);
    let mut cases = Vec::new();

    let mut wrong = base.clone();
    wrong.protocol_bundle_hash = B256::repeat_byte(0xA1);
    cases.push(("protocol bundle", wrong));
    let mut wrong = base.clone();
    wrong.attempt = wrong.attempt.saturating_add(1);
    cases.push(("attempt", wrong));
    let mut wrong = base.clone();
    wrong.result_validator_set_epoch = wrong.result_validator_set_epoch.saturating_add(1);
    cases.push(("ValidatorSet epoch", wrong));
    let mut wrong = base.clone();
    wrong.result_committee_set_hash = B256::repeat_byte(0xA2);
    cases.push(("consensus snapshot hash", wrong));
    let mut wrong = base.clone();
    wrong.result_ocomp_binding_hash = B256::repeat_byte(0xA3);
    cases.push(("OCOMP binding hash", wrong));
    let mut wrong = base.clone();
    wrong.validator_index = u16::MAX;
    cases.push(("participant index", wrong));
    let mut wrong = base.clone();
    wrong.key_epoch = wrong.key_epoch.saturating_add(1);
    cases.push(("key epoch", wrong));
    let mut wrong = base;
    wrong.signature_rs[0] ^= 1;
    cases.push(("signature", wrong));

    for (label, vote) in cases {
        assert!(
            matches!(
                submit_vote_result(&mut fixture, &vote, 14),
                Err(PrecompileError::RevertBytes(_))
            ),
            "{label} mismatch must be a public vote rejection"
        );
    }
}

#[test]
fn old_job_keeps_its_four_member_snapshot_across_a_four_to_five_boundary() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let next_snapshot = fixture.activate_additional_validator_for_test(4);
    assert_eq!(next_snapshot.member_count, 5);

    let old_participant_vote = fixture.signed_result_vote(3);
    assert_eq!(
        submit_vote_result(&mut fixture, &old_participant_vote, 14).unwrap(),
        Bytes::new()
    );

    let new_validator_vote = fixture.signed_result_vote(4);
    assert!(matches!(
        submit_vote_result(&mut fixture, &new_validator_vote, 15),
        Err(PrecompileError::RevertBytes(_))
    ));
}

#[test]
fn missing_pinned_snapshot_reverts_public_vote_without_changing_the_job() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let vote = fixture.signed_result_vote(0);
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        vote.result_validator_set_epoch,
        vote.result_committee_set_hash,
    );
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        validators
            .committee_snapshot_exists
            .write(&snapshot_key, false)
            .unwrap();
    });

    assert!(matches!(
        submit_vote_result(&mut fixture, &vote, 14),
        Err(PrecompileError::RevertBytes(_))
    ));
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let contract = MetadosisContract::new(storage);
        let record = contract
            .ocomp_job_record(fixture.intent_id, &fixture.limits)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::VotingOpen);
        let accountability = contract
            .result_vote_accountability(vote.job_id, &fixture.limits)
            .unwrap()
            .unwrap();
        assert!(accountability.slots.iter().all(Option::is_none));
        assert!(accountability.quorum.is_none());
    });
}
