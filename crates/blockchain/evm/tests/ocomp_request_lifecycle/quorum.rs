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
    let voting = ResultVotingScenario::for_intent(
        &voting_open.record.intent,
        finalized_record.finalized.as_ref().unwrap().job_id,
    );
    let voting_result = voting.result().clone();

    let signed_votes = (0_u8..3)
        .map(|validator_index| (validator_index, voting.signed_vote(validator_index)))
        .collect::<Vec<_>>();
    let mut voting_open_state = HashMapStorageProvider::new(CHAIN_ID);
    voting_open_state.storage = voting_open.storage.clone();
    StorageHandle::enter(&mut voting_open_state, |storage| {
        for (validator_index, vote) in &signed_votes {
            let prefix = vote.prefix();
            assert_eq!(
                outbe_metadosis::resolve_historical_result_vote_participant(
                    storage.clone(),
                    &prefix,
                    &poc_schema_limits(),
                )
                .expect("historical OCOMP vote participant resolution"),
                Some(validator_sender(*validator_index)),
            );
        }
    });
    let vote_transactions = signed_votes
        .into_iter()
        .map(|(validator_index, vote)| {
            let calldata = encode_submit_lysis_result_calldata(&vote, &poc_schema_limits())
                .expect("canonical q-forming vote calldata");
            pooled_vote_transaction(Bytes::from(calldata), validator_index)
        })
        .collect::<Vec<_>>();
    let vote_hashes = vote_transactions
        .iter()
        .map(PoolTransaction::hash)
        .copied()
        .collect::<Vec<_>>();
    let mut saturated_transactions = (0..SATURATED_USER_TRANSACTION_COUNT)
        .map(pooled_saturated_user_transaction)
        .collect::<Vec<_>>();
    // Deliberately insert the higher-tip user workload first. Production
    // OutbeTransactionOrdering must still select every OCOMP carrier ahead of it.
    saturated_transactions.extend(vote_transactions);
    let q_forming = build_canonical_ocomp_successor(
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
        saturated_transactions,
    );
    assert!(
        q_forming.user_transaction_count > vote_hashes.len(),
        "saturated block must contain the OCOMP carriers plus ordinary user work"
    );
    assert!(
        q_forming.user_transaction_count
            < usize::try_from(SATURATED_USER_TRANSACTION_COUNT).unwrap() + vote_hashes.len(),
        "offered user gas must exceed the block budget so priority is observable"
    );
    assert!(
        q_forming.user_transaction_hashes[..vote_hashes.len()]
            .iter()
            .all(|hash| vote_hashes.contains(hash)),
        "all OCOMP carriers must be selected before higher-tip ordinary transactions"
    );
    assert!(q_forming.user_receipt_successes[..vote_hashes.len()]
        .iter()
        .all(|success| *success));
    assert!(q_forming.user_receipt_successes[vote_hashes.len()..]
        .iter()
        .all(|success| !*success));
    assert!(q_forming.user_receipt_cumulative_gas[..vote_hashes.len()]
        .windows(2)
        .all(|window| window[0] == window[1]));
    assert!(
        q_forming.user_receipt_cumulative_gas.last().unwrap()
            > &q_forming.user_receipt_cumulative_gas[vote_hashes.len() - 1],
        "ordinary saturated transactions, unlike OCOMP carriers, consume user-lane gas"
    );
    assert_eq!(
        q_forming.record.status,
        OcompJobStatus::Completed,
        "quorum must complete before expiry: open_height={open_height}, record={:?}",
        q_forming.record
    );
    let completed = q_forming
        .record
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.completed_binding.as_ref())
        .expect("q-forming block persists completed binding")
        .clone();
    let quorum = q_forming
        .record
        .finalized
        .as_ref()
        .and_then(|finalized| finalized.quorum.as_ref())
        .expect("q-forming block persists quorum")
        .clone();
    assert_eq!(
        quorum.result_digest,
        voting_result.result_digest(&poc_schema_limits()).unwrap()
    );
    assert_eq!(quorum.signer_bitmap, vec![0b0111]);
    assert_eq!(completed.quorum_evidence_hash, quorum.evidence_hash);

    let mut completed_state = HashMapStorageProvider::new(CHAIN_ID);
    completed_state.storage = q_forming.storage;
    StorageHandle::enter(&mut completed_state, |storage| {
        let job_id = q_forming
            .record
            .finalized
            .as_ref()
            .expect("completed record remains finalized")
            .job_id;
        let accountability = OcompVoteAccountabilityV1::decode_canonical(
            &outbe_metadosis::api::get_offchain_vote_accountability(storage.clone(), job_id)
                .expect("public q-forming accountability"),
            &poc_schema_limits(),
        )
        .unwrap();
        assert_eq!(accountability.slots.iter().flatten().count(), 3);
        assert_eq!(accountability.quorum.as_ref(), Some(&quorum));

        let terminal_receipt = AggregateActivationReceiptV1::decode_canonical(
            &outbe_metadosis::api::get_lysis_terminal_receipt(storage.clone(), intent_id)
                .expect("public q-forming terminal receipt"),
            &poc_schema_limits(),
        )
        .unwrap();
        assert_eq!(completed.terminal_receipt, terminal_receipt);
        let generation = ActiveGenerationV1::decode_canonical(
            &outbe_metadosis::api::get_active_lysis_generation(storage.clone(), prepared.wwd)
                .expect("public q-forming active generation"),
            &poc_schema_limits(),
        )
        .unwrap();
        assert_eq!(generation.job_id, voting_result.job_id);
        assert_eq!(generation.nod_root, voting_result.roots.nod_root);
        assert_eq!(generation.exact_counts, voting_result.counts);
        let projection = outbe_metadosis::api::worldwide_day(storage, prepared.wwd)
            .unwrap()
            .unwrap();
        assert_eq!(projection.status, outbe_metadosis::WwdStatus::Completed);
        assert_eq!(
            projection.membership,
            outbe_metadosis::WwdMembership::Closed
        );
    });
}
