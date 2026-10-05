//! A result-vote carrier that passes stateless classification but carries an
//! invalid inner signature fails the same way on every node. The leader must
//! still publish its block: the builder leaves that carrier out, keeps the next
//! user transaction, and import replay must match the builder.

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
    let fixture = OcompSuccessorFixture {
        chain_spec: &chain_spec,
        tree_service: &prepared.tree_service,
        signer: &signer,
        runtime_body_readers: &runtime_body_readers,
        fork_install: &fork_install,
        dkg: &dkg,
        snapshot: &snapshot,
    };
    let voting = ResultVotingScenario::for_intent(
        &voting_open.record.intent,
        finalized_record.finalized.as_ref().unwrap().job_id,
    );
    let mut vote = voting.signed_vote(0);
    // Same job, committee and outer signer; only the inner vote signature is wrong.
    vote.signature_rs[63] ^= 0x01;
    let calldata = encode_submit_lysis_result_calldata(&vote, &poc_schema_limits())
        .expect("a tampered signature keeps the canonical carrier encoding");
    let invalid_carrier = pooled_vote_transaction(Bytes::from(calldata), 0);
    let valid = pooled_user_call(
        saturated_user_secret(),
        0,
        Address::repeat_byte(0x11),
        21_000,
        Bytes::new(),
    );
    let valid_hash = *PoolTransaction::hash(&valid);

    let height = open_height + 1;
    // The canonical successor returns only after the production builder, the
    // import replay, and the historical replay agree on the execution output.
    let built = build_canonical_ocomp_successor(
        fixture,
        OcompSuccessorBlock {
            proposer,
            parent: voting_open.header,
            parent_storage: &voting_open.storage,
            height,
            timestamp: prepared.request_time + (height - REQUEST_HEIGHT),
            intent_id,
            user_transactions: vec![invalid_carrier, valid],
        },
    );
    assert_eq!(
        built.user_transaction_hashes,
        vec![valid_hash],
        "the invalid carrier stays out of the block and the transfer is kept"
    );
    assert_eq!(built.user_receipt_successes, vec![true]);
    assert_eq!(built.record.status, OcompJobStatus::VotingOpen);
}
