use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_ocomp_protocol::{
    receipts::ActivationOutcome, state::OcompJobStatus, vote::OcompVoteAccountabilityV1,
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

fn submit_vote(fixture: &mut ActivationFixture, validator_index: u8, height: u64) {
    fixture.provider.set_block_number(height);
    fixture.provider.enable_lysis_activation_frame();
    fixture
        .provider
        .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::VerifiedResultVote);
    let vote = fixture.signed_result_vote(validator_index);
    let vote_bytes = vote.encode_canonical(&fixture.limits).unwrap();
    let calldata = IMetadosis::submitLysisResultCall {
        resultVoteV1: Bytes::from(vote_bytes),
    }
    .abi_encode();
    StorageHandle::enter(&mut fixture.provider, |storage| {
        assert_eq!(
            crate::commands::submit_verified_result_vote(
                storage,
                &fixture.scope,
                &calldata,
                U256::ZERO,
                false,
            )
            .unwrap(),
            Bytes::new()
        );
    });
}

// OCOMP-TEST-ID: OCM-VOT-001
#[test]
fn public_dispatch_records_four_slots_and_immutable_q3() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let job_id = fixture.result.job_id;
    let intent_id = fixture.intent_id;
    let expected_result_digest = result_digest(&fixture);

    for index in 0..4 {
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
        let quorum = record.finalized.unwrap().quorum.unwrap();
        assert_eq!(quorum.result_digest, expected_result_digest);
        assert_eq!(quorum.signer_bitmap, vec![0b0111]);

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
        assert_eq!(accountability.slots.iter().flatten().count(), 4);
        assert_eq!(accountability.quorum.unwrap(), quorum);
    });
}
