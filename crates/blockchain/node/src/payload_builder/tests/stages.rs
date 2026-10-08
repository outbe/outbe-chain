use super::*;
use crate::payload_builder::{
    execution::{PayloadBuildState, StageOutcome},
    preparation::{self, PayloadContext},
    selection::UserTransactions,
};
use alloy_primitives::Address;
use reth_evm::execute::BlockExecutor as _;
use reth_revm::cancelled::CancelOnDrop;
use revm::{
    database_interface::DBErrorMarker,
    state::{AccountInfo, Bytecode},
    Database,
};
use std::sync::Mutex;

struct UserStageResult {
    outcome: Result<StageOutcome, reth_payload_primitives::PayloadBuilderError>,
    receipts: Vec<bool>,
    invalid: Vec<InvalidPoolTransactionError>,
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
    run_user_stage_with_options(
        transactions,
        reserved_end_gas,
        cancel,
        StageOptions::default(),
    )
}

#[derive(Default)]
struct StageOptions {
    code: Bytes,
    fail_storage: bool,
    account_nonce: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
#[error("node storage read failed: {0}")]
struct StageReadError(String);
impl DBErrorMarker for StageReadError {}

/// Faults the actual EVM database read, not the selection/error-policy code.
#[derive(Debug)]
struct StageDatabase<DB> {
    inner: DB,
    fail_storage: bool,
}
impl<DB: Database> Database for StageDatabase<DB> {
    type Error = StageReadError;
    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.inner
            .basic(address)
            .map_err(|e| StageReadError(e.to_string()))
    }
    fn code_by_hash(&mut self, hash: B256) -> Result<Bytecode, Self::Error> {
        self.inner
            .code_by_hash(hash)
            .map_err(|e| StageReadError(e.to_string()))
    }
    fn storage(&mut self, address: Address, key: U256) -> Result<U256, Self::Error> {
        if self.fail_storage && address == address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC") {
            return Err(StageReadError("unavailable".into()));
        }
        self.inner
            .storage(address, key)
            .map_err(|e| StageReadError(e.to_string()))
    }
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.inner
            .block_hash(number)
            .map_err(|e| StageReadError(e.to_string()))
    }
}

struct RecordingTransactions {
    transactions: std::vec::IntoIter<Arc<ValidPoolTransaction<EthPooledTransaction>>>,
    invalid: Arc<Mutex<Vec<InvalidPoolTransactionError>>>,
}
impl Iterator for RecordingTransactions {
    type Item = Arc<ValidPoolTransaction<EthPooledTransaction>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.transactions.next()
    }
}
impl BestTransactions for RecordingTransactions {
    fn mark_invalid(&mut self, _: &Self::Item, reason: InvalidPoolTransactionError) {
        self.invalid.lock().unwrap().push(reason);
    }
    fn no_updates(&mut self) {}
    fn set_skip_blobs(&mut self, _: bool) {}
}

fn run_user_stage_with_options(
    transactions: &[(u64, u64)],
    reserved_end_gas: u64,
    cancel: CancelOnDrop,
    options: StageOptions,
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
    if !options.code.is_empty() {
        case.provider.add_account(
            address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"),
            ExtendedAccount::new(0, U256::ZERO).with_bytecode(options.code.clone()),
        );
    }
    let mut db = State::builder()
        .with_database(StageDatabase {
            inner: StateProviderDatabase::new(&case.provider),
            fail_storage: options.fail_storage,
        })
        .with_bundle_update()
        .build();
    let mut builder = case
        .evm_config
        .builder_for_next_block(&mut db, &parent, env)
        .expect("user-stage builder initializes");
    preparation::apply_pre_execution_changes(&mut builder).expect("pre-execution succeeds");
    let invalid = Arc::new(Mutex::new(Vec::new()));
    let mut candidates = Vec::new();
    let transactions = transactions
        .iter()
        .enumerate()
        .map(|(index, &(nonce, gas_limit))| {
            let tx: OutbeTxEnvelope = TxEip1559 {
                chain_id: chain_spec.chain().id(),
                nonce,
                gas_limit,
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 7,
                to: TxKind::Call(if index > 0 && !options.code.is_empty() {
                    Address::repeat_byte(0xdd)
                } else {
                    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
                }),
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
                ExtendedAccount::new(
                    options.account_nonce.unwrap_or(nonce),
                    U256::from(100_000_000_000_000_000_000u128),
                ),
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
    let mut best_txs = RecordingTransactions {
        transactions: transactions.into_iter(),
        invalid: invalid.clone(),
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
    .execute(&mut builder);
    let receipts = builder
        .executor_mut()
        .receipts()
        .iter()
        .map(|r| r.success)
        .collect();
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
    let invalid = invalid.lock().unwrap().drain(..).collect::<Vec<_>>();
    UserStageResult {
        outcome,
        receipts,
        included_nonces: included.iter().map(|&(_, nonce, _)| nonce).collect(),
        gas_used: state.cumulative_gas_used,
        fees: state.total_fees,
        body_bytes,
        rejected: invalid.len(),
        invalid,
    }
}

#[test]
fn user_stage_accounts_only_included_transactions_and_skips_nonce_too_low() {
    let result = run_user_stage(
        &[(0, 30_000), (0, 30_000), (1, 30_000)],
        0,
        Default::default(),
    );
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.included_nonces, vec![0, 1]);
    assert_eq!(result.gas_used, 42_000);
    assert_eq!(result.fees, U256::from(42_000u64 * 7));
    assert!(result.body_bytes > 0);
    assert_eq!(result.rejected, 0, "nonce-too-low does not poison the pool");
}

#[test]
fn user_stage_reserves_terminal_gas_before_execution() {
    let result = run_user_stage(&[(0, 100_000)], 1, Default::default());
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
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
    assert_eq!(result.outcome.unwrap(), StageOutcome::Cancelled);
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
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert!(result.included_nonces.is_empty());
    assert_eq!(result.rejected, 1);
    assert_eq!(result.gas_used, 0);
    assert_eq!(result.fees, U256::ZERO);
    assert_eq!(result.body_bytes, 0);
}

#[test]
fn user_revert_is_included_with_failed_receipt_and_consumed_nonce() {
    let result = run_user_stage_with_options(
        &[(0, 30_000), (0, 30_000)],
        0,
        Default::default(),
        StageOptions {
            code: Bytes::from_static(&[0x5f, 0x5f, 0xfd]),
            ..Default::default()
        },
    );
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.receipts, vec![false, true]);
    assert_eq!(result.included_nonces, vec![0, 0]);
    assert_eq!(result.rejected, 0);
}

#[test]
fn node_storage_failure_aborts_selection_without_receipt_or_pool_invalidation() {
    let result = run_user_stage_with_options(
        &[(0, 30_000), (0, 30_000)],
        0,
        Default::default(),
        StageOptions {
            code: Bytes::from_static(&[0x5f, 0x54, 0x00]),
            fail_storage: true,
            ..Default::default()
        },
    );
    assert!(result.outcome.is_err());
    assert!(result.receipts.is_empty());
    assert!(result.included_nonces.is_empty());
    assert_eq!(result.rejected, 0);
    assert_eq!(result.gas_used, 0);
    assert_eq!(result.fees, U256::ZERO);
    assert_eq!(result.body_bytes, 0);
}

#[test]
fn invalid_transaction_preserves_the_actual_validation_reason() {
    let result = run_user_stage_with_options(
        &[(4, 30_000)],
        0,
        Default::default(),
        StageOptions {
            account_nonce: Some(0),
            ..Default::default()
        },
    );
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.invalid.len(), 1);
    let InvalidPoolTransactionError::Other(error) = &result.invalid[0] else {
        panic!(
            "expected original typed validation reason, got {}",
            result.invalid[0]
        );
    };
    let preserved = error
        .as_any()
        .downcast_ref::<crate::payload_builder::transaction_error::PreservedInvalidUserTx>()
        .unwrap();
    assert!(matches!(
        preserved.reason.as_invalid_tx_err(),
        Some(revm::context_interface::result::InvalidTransaction::NonceTooHigh { tx: 4, state: 0 })
    ));
}

#[test]
fn local_read_deadline_is_a_fatal_precompile_error() {
    let result = outbe_evm::precompiles::map_outbe_precompile_result(
        Err(PrecompileError::BodyReadRequestDeadline),
        100,
    );
    assert!(matches!(
        result,
        Err(revm::precompile::PrecompileError::Fatal(_))
    ));
}
