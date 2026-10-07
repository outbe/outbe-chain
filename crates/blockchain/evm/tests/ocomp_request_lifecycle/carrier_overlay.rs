//! The carrier check must see the block state left by earlier transactions in
//! the same block, not the parent state. Here the validator first assigns an
//! OCOMP delegate, which revokes its own authority to sign carriers, and then
//! submits a carrier signed with its own key. On the parent state that carrier
//! is valid. On the block state it is not, and execution would reject it.

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
    let delegate = Address::repeat_byte(0xD0);
    let assign_delegate = pooled_user_call(
        validator_secret(0),
        0,
        VALIDATOR_SET_ADDRESS,
        300_000,
        outbe_validatorset::precompile::IValidatorSet::setDelegateCall {
            role: outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp.id(),
            delegate,
        }
        .abi_encode()
        .into(),
    );
    let assign_delegate_hash = *PoolTransaction::hash(&assign_delegate);
    let calldata =
        encode_submit_lysis_result_calldata(&voting.signed_vote(0), &poc_schema_limits())
            .expect("canonical vote calldata");
    // Same sender, next nonce: the pool cannot order the carrier first.
    let self_signed_carrier = pooled_vote_transaction_at_nonce(Bytes::from(calldata), 0, 1);

    let height = open_height + 1;
    let built = build_canonical_ocomp_successor(
        fixture,
        OcompSuccessorBlock {
            proposer,
            parent: voting_open.header,
            parent_storage: &voting_open.storage,
            height,
            timestamp: prepared.request_time + (height - REQUEST_HEIGHT),
            intent_id,
            user_transactions: vec![assign_delegate, self_signed_carrier],
        },
    );

    assert_eq!(
        built.user_transaction_hashes,
        vec![assign_delegate_hash],
        "the delegate assignment lands and the now-unauthorized carrier is left out"
    );
    assert_eq!(built.user_receipt_successes, vec![true]);
    let mut state = HashMapStorageProvider::new(CHAIN_ID);
    state.storage = built.storage;
    StorageHandle::enter(&mut state, |storage| {
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(
            validators
                .get_delegate(
                    validator_sender(0),
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                )
                .unwrap(),
            delegate
        );
    });
}
