use super::*;
use crate::payload_builder::{
    execution::{PayloadBuildState, StageOutcome},
    preparation::{self, PayloadContext},
    selection::UserTransactions,
};
use reth_revm::cancelled::CancelOnDrop;
use revm::Database as _;

struct UserStageResult {
    outcome: StageOutcome,
    included_nonces: Vec<u64>,
    gas_used: u64,
    fees: U256,
    body_bytes: usize,
    rejected: usize,
}

/// Exercise pool selection against the real EVM. This helper disables block hooks to
/// isolate this stage from the bootstrap zone that the replay tests cover.
fn run_user_stage(
    transactions: &[(u64, u64)],
    reserved_end_gas: u64,
    cancel: CancelOnDrop,
) -> UserStageResult {
    let case = build_active_payload_case(0, 1);
    let chain_spec = case.provider.chain_spec();
    let parent = SealedHeader::seal_slow(OutbeHeader::new(alloy_consensus::Header {
        gas_limit: 100_000,
        timestamp: ACTIVE_PAYLOAD_BLOCK_TIMESTAMP - 1,
        base_fee_per_gas: Some(1_000_000_000),
        ..Default::default()
    }));
    let attributes = OutbePayloadAttributes::new(outbe_primitives::OutbePayloadAttributesInput {
        suggested_fee_recipient: REWARDS_ADDRESS,
        timestamp_millis: ACTIVE_PAYLOAD_BLOCK_TIMESTAMP * 1000,
        prev_randao: B256::repeat_byte(0x44),
        parent_beacon_block_root: None,
        extra_data: Bytes::new(),
        parent_consensus_metadata: None,
        proposer_evm_address: None,
    });
    let context = PayloadContext {
        parent: &parent,
        attributes: &attributes,
        chain_spec: &chain_spec,
    };
    let mut env = preparation::prepare(&case.evm_config, &context)
        .expect("execution attributes prepare")
        .env;
    env.execute_outbe_block_hooks = false;
    let mut db = State::builder()
        .with_database(StateProviderDatabase::new(&case.provider))
        .with_bundle_update()
        .build();
    let mut builder = case
        .evm_config
        .builder_for_next_block(&mut db, &parent, env)
        .expect("user-stage builder initializes");
    preparation::apply_pre_execution_changes(&mut builder).expect("pre-execution succeeds");
    let rejected = Arc::new(AtomicUsize::new(0));
    let mut candidates = Vec::new();
    let transactions = transactions
        .iter()
        .map(|&(nonce, gas_limit)| {
            let tx: OutbeTxEnvelope = TxEip1559 {
                chain_id: chain_spec.chain().id(),
                nonce,
                gas_limit,
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 7,
                to: TxKind::Call(address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")),
                value: U256::ZERO,
                ..Default::default()
            }
            .into_signed(Signature::test_signature())
            .into();
            let encoded_length = tx.encode_2718_len();
            let recovered = tx.try_into_recovered().expect("user signature recovers");
            // Different signed messages recover different addresses with the
            // synthetic signature. Fund each sender at its requested nonce.
            let sender = alloy_primitives::Address::from(*recovered.signer());
            if !candidates.iter().any(|&(seen, _, _)| seen == sender) {
                candidates.push((
                    sender,
                    nonce,
                    alloy_rlp::Encodable::length(recovered.inner()),
                ));
            }
            case.provider.add_account(
                sender,
                ExtendedAccount::new(nonce, U256::from(100_000_000_000_000_000_000u128)),
            );
            Arc::new(ValidPoolTransaction {
                transaction: EthPooledTransaction::new(recovered, encoded_length),
                transaction_id: TransactionId::new(SenderId::from(1), nonce),
                propagate: true,
                timestamp: Instant::now(),
                origin: TransactionOrigin::Local,
                authority_ids: None,
            })
        })
        .collect::<Vec<_>>();
    let mut best_txs = TestBestTransactions {
        transactions: transactions.into_iter(),
        rejected: rejected.clone(),
    };
    let mut state = PayloadBuildState::new(
        &context,
        &EthereumBuilderConfig::new(),
        100_000,
        builder.evm_mut().block().basefee(),
        &[],
    )
    .expect("budgets initialize");
    state.reserved_end_gas = reserved_end_gas;
    let initial_size = state.size_budget.estimate(0).unwrap();
    let pool = TestPool::new();
    let outcome = UserTransactions {
        pool: &pool,
        best_txs: &mut best_txs,
        state: &mut state,
        cancel: &cancel,
        payload_id: PayloadId::new([0x09; 8]),
        carrier_block: None,
    }
    .execute(&mut builder)
    .expect("selection completes without an execution failure");
    let body_bytes = state.size_budget.estimate(0).unwrap() - initial_size;
    drop(builder);
    let included = candidates
        .into_iter()
        .filter(|&(sender, nonce, _)| {
            db.basic(sender)
                .expect("post-execution account is readable")
                .expect("funded user exists")
                .nonce
                == nonce + 1
        })
        .collect::<Vec<_>>();
    assert_eq!(
        body_bytes,
        included
            .iter()
            .map(|&(_, _, encoded_length)| encoded_length)
            .sum::<usize>(),
        "size accounting must match only the users whose execution committed"
    );
    UserStageResult {
        outcome,
        included_nonces: included.iter().map(|&(_, nonce, _)| nonce).collect(),
        gas_used: state.cumulative_gas_used,
        fees: state.total_fees,
        body_bytes,
        rejected: rejected.load(Ordering::Relaxed),
    }
}

#[test]
fn user_stage_accounts_only_included_transactions_and_skips_nonce_too_low() {
    let result = run_user_stage(
        &[(0, 30_000), (0, 30_000), (1, 30_000)],
        0,
        Default::default(),
    );
    assert_eq!(result.outcome, StageOutcome::Completed);
    assert_eq!(result.included_nonces, vec![0, 1]);
    assert_eq!(result.gas_used, 42_000);
    assert_eq!(result.fees, U256::from(42_000u64 * 7));
    assert!(result.body_bytes > 0);
    assert_eq!(result.rejected, 0, "nonce-too-low does not poison the pool");
}

#[test]
fn user_stage_reserves_terminal_gas_before_execution() {
    let result = run_user_stage(&[(0, 100_000)], 1, Default::default());
    assert_eq!(result.outcome, StageOutcome::Completed);
    assert!(result.included_nonces.is_empty());
    assert_eq!(result.rejected, 1);
    assert_eq!(result.gas_used, 0);
    assert_eq!(result.fees, U256::ZERO);
    assert_eq!(result.body_bytes, 0);
}

#[test]
fn user_stage_cancellation_leaves_candidate_unexecuted_and_unaccounted() {
    let cancel = CancelOnDrop::default();
    drop(cancel.clone());
    let result = run_user_stage(&[(0, 30_000)], 0, cancel);
    assert_eq!(result.outcome, StageOutcome::Cancelled);
    assert!(result.included_nonces.is_empty());
    assert_eq!(result.rejected, 0);
    assert_eq!(result.gas_used, 0);
    assert_eq!(result.fees, U256::ZERO);
    assert_eq!(result.body_bytes, 0);
}

#[test]
fn user_stage_gas_rejection_precedes_cancellation_checkpoint() {
    let cancel = CancelOnDrop::default();
    drop(cancel.clone());
    let result = run_user_stage(&[(0, 100_001)], 0, cancel);
    assert_eq!(result.outcome, StageOutcome::Completed);
    assert!(result.included_nonces.is_empty());
    assert_eq!(result.rejected, 1);
    assert_eq!(result.gas_used, 0);
    assert_eq!(result.fees, U256::ZERO);
    assert_eq!(result.body_bytes, 0);
}
