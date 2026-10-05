//! When the builder cannot read the state it needs to judge a result-vote
//! carrier, the fault belongs to this node, not to the carrier. The build must
//! fail so the leader proposes nothing, instead of skipping the carrier or
//! publishing a block that leaves it out.

use super::*;

pub(crate) fn run() {
    let (
        environment,
        VotingOpenState {
            prepared,
            proposer,
            open_height,
            intent_id,
            finalized_record,
            voting_open,
        },
    ) = super::request::open_voting().into_successor_parts();
    let fixture = environment.fixture(&prepared.tree_service);
    let voting = ResultVotingScenario::for_intent(
        &voting_open.record.intent,
        finalized_record.finalized.as_ref().unwrap().job_id,
    );
    let calldata =
        encode_submit_lysis_result_calldata(&voting.signed_vote(0), &poc_schema_limits())
            .expect("canonical vote calldata");
    let carrier = pooled_vote_transaction(Bytes::from(calldata), 0);
    let transfer = pooled_user_call(
        saturated_user_secret(),
        0,
        Address::repeat_byte(0x11),
        21_000,
        Bytes::new(),
    );

    // The carrier check authorizes the outer signer through the validator's
    // OCOMP delegate slot. Nothing earlier in the block reads that slot.
    let delegate_slot = {
        let mut state = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut state, |storage| {
            outbe_validatorset::contract::ValidatorSet::new(storage)
                .delegate_by_validator_role
                .get_nested(&validator_sender(0))
                .get(&outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp.id())
                .slot()
        })
    };

    let height = open_height + 1;
    let outcome = try_build_canonical_ocomp_successor(
        fixture,
        OcompSuccessorBlock {
            proposer,
            parent: voting_open.header,
            parent_storage: &voting_open.storage,
            height,
            timestamp: prepared.request_time + (height - REQUEST_HEIGHT),
            intent_id,
            user_transactions: vec![carrier, transfer],
        },
        Some((
            VALIDATOR_SET_ADDRESS,
            B256::from(delegate_slot.to_be_bytes::<32>()),
        )),
    );

    let error = match outcome {
        Ok(_) => panic!("an unreadable carrier check must not publish a block"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("result-vote carrier state is unavailable"),
        "the build fails at the carrier check, not later in execution: {error}"
    );
}
