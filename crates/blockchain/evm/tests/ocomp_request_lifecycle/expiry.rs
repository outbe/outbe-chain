use super::*;

pub(crate) fn run() {
    let VotingOpenScenario {
        chain_spec,
        prepared,
        signer,
        runtime_body_readers,
        fork_install,
        dkg,
        snapshot,
        proposer,
        open_height,
        intent_id,
        finalized_record,
        voting_open,
    } = super::request::open_voting();
    let initial_deadline = voting_open
        .record
        .finalized
        .as_ref()
        .expect("initial voting-open record remains finalized")
        .deadline_height;
    let initial_voting = ResultVotingScenario::for_intent(
        &finalized_record.intent,
        finalized_record.finalized.as_ref().unwrap().job_id,
    );
    let initial_votes = (0_u8..2)
        .map(|validator_index| {
            let vote = initial_voting.signed_vote(validator_index);
            let calldata = encode_submit_lysis_result_calldata(&vote, &poc_schema_limits())
                .expect("canonical non-quorum vote calldata");
            pooled_vote_transaction(Bytes::from(calldata), validator_index)
        })
        .collect::<Vec<_>>();
    let no_quorum = build_canonical_ocomp_successor(
        &chain_spec,
        &prepared.tree_service,
        &signer,
        &runtime_body_readers,
        &fork_install,
        &dkg,
        &snapshot,
        proposer,
        voting_open.header,
        &voting_open.storage,
        open_height + 1,
        prepared.request_time + (open_height + 1 - REQUEST_HEIGHT),
        intent_id,
        initial_votes,
    );
    assert_eq!(no_quorum.record.status, OcompJobStatus::VotingOpen);
    assert!(no_quorum
        .record
        .finalized
        .as_ref()
        .is_some_and(|record| record.quorum.is_none()));

    // Validator 2 has not voted. Its vote arrives in the deadline block itself,
    // after the begin zone has closed the window, so execution records the
    // late-vote soft failure instead of rejecting the block.
    let late_calldata =
        encode_submit_lysis_result_calldata(&initial_voting.signed_vote(2), &poc_schema_limits())
            .expect("canonical late vote calldata");
    let late_carrier = pooled_vote_transaction(Bytes::from(late_calldata), 2);
    let late_carrier_hash = *PoolTransaction::hash(&late_carrier);
    let mut late_carrier = Some(late_carrier);

    let mut expiry_parent = no_quorum.header;
    let mut expiry_storage = no_quorum.storage;
    let mut initial_terminal = no_quorum.record;
    for height in (open_height + 2)..=initial_deadline {
        let deadline_block = height == initial_deadline;
        let pool = if deadline_block {
            late_carrier.take().into_iter().collect()
        } else {
            Vec::new()
        };
        let built = build_canonical_ocomp_successor(
            &chain_spec,
            &prepared.tree_service,
            &signer,
            &runtime_body_readers,
            &fork_install,
            &dkg,
            &snapshot,
            proposer,
            expiry_parent,
            &expiry_storage,
            height,
            prepared.request_time + (height - REQUEST_HEIGHT),
            intent_id,
            pool,
        );
        if deadline_block {
            assert_eq!(
                built.user_transaction_hashes,
                vec![late_carrier_hash],
                "the late carrier is included in the deadline block"
            );
            assert_eq!(
                built.user_receipt_successes,
                vec![false],
                "the late carrier gets the deadline-passed soft-failure receipt"
            );
        }
        expiry_parent = built.header;
        expiry_storage = built.storage;
        initial_terminal = built.record;
    }
    assert_eq!(initial_terminal.status, OcompJobStatus::Expired);
    let initial_terminal_evidence = initial_terminal
        .terminal
        .as_ref()
        .expect("deadline retains the initial terminal evidence");
    assert_eq!(
        initial_terminal_evidence.outcome,
        OcompTerminalOutcome::Expired
    );
    assert_eq!(initial_terminal_evidence.terminal_height, initial_deadline);
    assert!(initial_terminal_evidence.completed_binding.is_none());

    // Expiry is terminal. Keep executing canonical blocks and check the
    // public projection, including the absence of a successor or Lysis output.
    for height in (initial_deadline + 1)..=(initial_deadline + 3) {
        let built = build_canonical_ocomp_successor(
            &chain_spec,
            &prepared.tree_service,
            &signer,
            &runtime_body_readers,
            &fork_install,
            &dkg,
            &snapshot,
            proposer,
            expiry_parent,
            &expiry_storage,
            height,
            prepared.request_time + (height - REQUEST_HEIGHT),
            intent_id,
            Vec::new(),
        );
        assert!(
            built.requested_intents.is_empty(),
            "expired day must not request a retry"
        );
        assert_eq!(
            built.record, initial_terminal,
            "terminal evidence must remain immutable"
        );
        let mut state = HashMapStorageProvider::new(CHAIN_ID);
        state.storage = built.storage.clone();
        StorageHandle::enter(&mut state, |storage| {
            let projection = outbe_metadosis::api::worldwide_day(storage.clone(), prepared.wwd)
                .unwrap()
                .unwrap();
            assert_eq!(projection.status, outbe_metadosis::WwdStatus::Failed);
            assert_eq!(
                projection.membership,
                outbe_metadosis::WwdMembership::Closed
            );
            assert!(matches!(
                outbe_metadosis::api::get_active_lysis_generation(storage.clone(), prepared.wwd),
                Err(outbe_primitives::error::PrecompileError::Revert(reason))
                    if reason == "ActiveGenerationV1 not found"
            ));
            assert!(matches!(
                outbe_metadosis::api::get_lysis_terminal_receipt(storage, intent_id),
                Err(outbe_primitives::error::PrecompileError::Revert(reason))
                    if reason == "AggregateActivationReceiptV1 not found"
            ));
        });
        expiry_parent = built.header;
        expiry_storage = built.storage;
    }
}
