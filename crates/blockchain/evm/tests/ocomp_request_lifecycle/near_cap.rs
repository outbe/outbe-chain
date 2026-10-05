//! A pool larger than one consensus message must still yield a block that the
//! validator's exact transport-size check accepts. The builder stops at its
//! reserved estimate, so the sealed block, including its finalized header
//! artifacts, stays within `OUTBE_MAX_BLOCK_SIZE`.

use super::*;
use outbe_primitives::consensus::{OUTBE_MAX_BLOCK_SIZE, OUTBE_MAX_EXTRA_DATA_SIZE};
use reth_ethereum::consensus::Consensus as _;

const CALLDATA_BYTES: usize = 110_000;
const TRANSACTION_COUNT: u64 = 18;
const TRANSACTION_GAS: u64 = 1_200_000;

pub(crate) fn run() {
    let (
        environment,
        VotingOpenState {
            prepared,
            proposer,
            open_height,
            intent_id,
            voting_open,
            ..
        },
    ) = super::request::open_voting().into_successor_parts();
    let fixture = environment.fixture(&prepared.tree_service);
    // Together the transactions exceed the transport cap.
    let transactions = (0..TRANSACTION_COUNT)
        .map(|nonce| {
            pooled_user_call(
                saturated_user_secret(),
                nonce,
                Address::repeat_byte(0x11),
                TRANSACTION_GAS,
                Bytes::from(vec![0_u8; CALLDATA_BYTES]),
            )
        })
        .collect::<Vec<_>>();
    let offered_bytes: usize = transactions
        .iter()
        .map(PoolTransaction::encoded_length)
        .sum();
    assert!(offered_bytes > OUTBE_MAX_BLOCK_SIZE);

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
            user_transactions: transactions,
        },
    );

    let sealed_length = built.sealed_block.rlp_length();
    assert!(
        built.user_transaction_count < usize::try_from(TRANSACTION_COUNT).unwrap(),
        "the size budget must leave part of the oversized pool out"
    );
    assert!(sealed_length <= OUTBE_MAX_BLOCK_SIZE);
    // One more transaction would still fit the gas limit, so the size budget,
    // not gas, left the rest of the pool out.
    let header = built.sealed_block.header();
    assert!(header.gas_used() + TRANSACTION_GAS + 1_000_000 <= header.gas_limit());
    // The builder packs up to the reserved estimate: at most the extra-data
    // reserve, header allowance and one more transaction remain unused.
    assert!(
        sealed_length + OUTBE_MAX_EXTRA_DATA_SIZE + 1024 + CALLDATA_BYTES + 1024
            > OUTBE_MAX_BLOCK_SIZE,
        "sealed block {sealed_length} bytes leaves more room than the reserve explains"
    );

    let validator = outbe_node::OutbeBeaconConsensus::new(environment.chain_spec.clone())
        .with_max_extra_data_size(OUTBE_MAX_EXTRA_DATA_SIZE)
        .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(PARENT_HEIGHT));
    validator
        .validate_block_pre_execution(&built.sealed_block)
        .expect("the validator accepts the near-cap block the builder sealed");
}
