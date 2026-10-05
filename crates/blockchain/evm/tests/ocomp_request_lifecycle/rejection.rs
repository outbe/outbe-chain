//! A user call that a precompile rejects must stay inside its own receipt.
//! The next user transaction in the same built block still executes, and
//! import replay must match the builder on receipts and state root.

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
        voting_open,
        ..
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
    let rejected_input = SystemTxInputV2::CycleTick
        .encode()
        .expect("cycle tick input encodes");
    let rejected_gas_limit = 21_000
        + rejected_input
            .iter()
            .map(|byte| if *byte == 0 { 4 } else { 16 })
            .sum::<u64>()
        + outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
        + 50;
    let rejected = pooled_user_call(
        saturated_user_secret(),
        0,
        outbe_primitives::addresses::STAKING_ADDRESS,
        rejected_gas_limit,
        rejected_input,
    );
    let valid = pooled_user_call(
        saturated_user_secret(),
        1,
        Address::repeat_byte(0x11),
        21_000,
        Bytes::new(),
    );
    let rejected_hash = *PoolTransaction::hash(&rejected);
    let valid_hash = *PoolTransaction::hash(&valid);
    let parent_state_root = voting_open.header.header().state_root();
    let height = open_height + 1;
    // The canonical successor returns only after the production builder, the
    // import replay, and the historical replay agree on the execution output,
    // including receipts, and the header state root matches that output.
    let built = build_canonical_ocomp_successor(
        fixture,
        OcompSuccessorBlock {
            proposer,
            parent: voting_open.header,
            parent_storage: &voting_open.storage,
            height,
            timestamp: prepared.request_time + (height - REQUEST_HEIGHT),
            intent_id,
            user_transactions: vec![rejected, valid],
        },
    );
    assert_eq!(
        built.user_transaction_hashes,
        vec![rejected_hash, valid_hash],
        "the rejected precompile call stays in the block ahead of the valid transfer"
    );
    assert_eq!(built.user_receipt_successes, vec![false, true]);
    assert!(
        built.user_receipt_cumulative_gas[1] > built.user_receipt_cumulative_gas[0],
        "the following transfer must execute and consume user-lane gas"
    );
    assert_ne!(
        built.header_state_root, parent_state_root,
        "the block must publish the post-state root agreed by builder and replay"
    );
    assert_eq!(built.record.status, OcompJobStatus::VotingOpen);
}
