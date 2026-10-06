//! A validator may finish its result before the response window opens. The
//! pool checks a vote against the canonical head for inclusion in the next
//! block. On the committed state after block `open_height - 1`, the window is
//! not open yet. The begin zone of the `open_height` block opens it before any
//! user transaction, so an honest vote for that block is valid. One block
//! earlier the vote cannot execute in the next block, so it is early: temporary,
//! never a bad transaction. These votes all stay invalid:
//! - a vote for a job that does not exist
//! - a vote with a wrong inner signature
//! - a vote carried by a signer the validator never authorized

use super::*;
use outbe_metadosis::api::{verify_result_vote_carrier, ResultVoteCarrierAdmission};

fn admit(
    committed: &HashMap<(Address, U256), U256>,
    vote: &outbe_ocomp_protocol::vote::ResultVoteV1,
    outer_signer: Address,
    inclusion_height: u64,
) -> ResultVoteCarrierAdmission {
    let calldata = encode_submit_lysis_result_calldata(vote, &poc_schema_limits())
        .expect("canonical vote calldata");
    let mut state = HashMapStorageProvider::new(CHAIN_ID);
    state.storage = committed.clone();
    StorageHandle::enter(&mut state, |storage| {
        verify_result_vote_carrier(
            storage,
            &calldata,
            outer_signer,
            inclusion_height,
            &poc_schema_limits(),
        )
    })
}

pub(crate) fn run() {
    let (
        VotingOpenScenario {
            state:
                VotingOpenState {
                    open_height,
                    finalized_record,
                    voting_open,
                    ..
                },
            ..
        },
        pre_open,
    ) = super::request::open_voting_with_pre_open_state();
    let voting = ResultVotingScenario::for_intent(
        &voting_open.record.intent,
        finalized_record.finalized.as_ref().unwrap().job_id,
    );
    let honest = voting.signed_vote(0);
    let mut unknown_job = honest.clone();
    unknown_job.job_id = B256::repeat_byte(0x99);
    let mut tampered = honest.clone();
    tampered.signature_rs[63] ^= 0x01;
    let one_block_before = &pre_open.one_block_before;

    let next_block = admit(one_block_before, &honest, validator_sender(0), open_height);
    assert!(
        matches!(
            next_block,
            ResultVoteCarrierAdmission::Valid { represented_validator }
                if represented_validator == validator_sender(0)
        ),
        "an honest vote for the block that opens its window is valid: {next_block:?}"
    );

    let early = admit(
        &pre_open.two_blocks_before,
        &honest,
        validator_sender(0),
        open_height - 1,
    );
    assert!(
        matches!(
            early,
            ResultVoteCarrierAdmission::NotYetOpen { open_height: opens } if opens == open_height
        ),
        "an honest vote two blocks before its window opens is temporary: {early:?}"
    );

    let unknown = admit(
        one_block_before,
        &unknown_job,
        validator_sender(0),
        open_height,
    );
    assert!(
        matches!(unknown, ResultVoteCarrierAdmission::InvalidCarrier { .. }),
        "a vote for a job that does not exist stays invalid: {unknown:?}"
    );
    let bad_signature = admit(
        one_block_before,
        &tampered,
        validator_sender(0),
        open_height,
    );
    assert!(
        matches!(
            bad_signature,
            ResultVoteCarrierAdmission::InvalidCarrier { .. }
        ),
        "a vote with a wrong inner signature stays invalid: {bad_signature:?}"
    );
    // Validator 1 is a committee member, but not validator 0's OCOMP delegate.
    let foreign_signer = admit(one_block_before, &honest, validator_sender(1), open_height);
    assert!(
        matches!(
            foreign_signer,
            ResultVoteCarrierAdmission::InvalidCarrier { .. }
        ),
        "a vote carried by an unauthorized signer stays invalid: {foreign_signer:?}"
    );
}
