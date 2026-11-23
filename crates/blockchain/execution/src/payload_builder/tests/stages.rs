use super::*;
use crate::payload_builder::{
    execution::{PayloadBuildState, StageOutcome},
    preparation::{self, PayloadContext},
    selection::UserTransactions,
};
use alloy_consensus::transaction::TxHashRef as _;
use alloy_primitives::Address;
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, Database},
    interpreter::Word,
};
use reth_evm::{
    execute::{
        BlockBuilderOutcome, BlockExecutionOutput, BlockExecutor, CommitChanges, ExecutorTx,
        GasOutput,
    },
    Evm, RecoveredTx as _, StateUpdate,
};
use reth_storage_api::AccountReader as _;
use reth_tasks::CancelOnDrop;
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
    payload: Option<OutbeBuiltPayload>,
}

/// Exercise pool selection against the real EVM. This helper disables block hooks to
/// isolate this stage from the bootstrap zone that the replay tests cover.
fn run_user_stage(transactions: &[(u64, u64)], cancel: CancelOnDrop) -> UserStageResult {
    run_user_stage_with_options(transactions, cancel, StageOptions::default())
}

#[derive(Default)]
struct StageOptions {
    code: Bytes,
    fail_storage: bool,
    account_nonce: Option<u64>,
    defer_first: bool,
    gas_deferral: bool,
    witness_deferral: bool,
    full_size: bool,
    blob: bool,
    finalize: bool,
    sparse_failure: bool,
}

/// Inject one executor deferral while keeping selection and subsequent EVM execution real.
struct DeferringBuilder<B> {
    inner: B,
    defer_next: bool,
    gas_deferral: bool,
    witness_deferral: bool,
}

impl<B: BlockBuilder> BlockBuilder for DeferringBuilder<B> {
    type Primitives = B::Primitives;
    type Executor = B::Executor;
    type EvmEnv = B::EvmEnv;

    fn evm_env(&self) -> &Self::EvmEnv {
        self.inner.evm_env()
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &evm2::TxResult<<<Self::Executor as BlockExecutor>::Evm as Evm>::EvmTypes>,
            evm2::TxStateChanges<'_>,
        ) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (env, tx) = tx.into_parts();
        if std::mem::take(&mut self.defer_next) {
            if self.gas_deferral {
                return Err(reth_errors::BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: 50_000,
                    block_available_gas: 49_999,
                }.into());
            }
            return Err(if self.witness_deferral {
                outbe_evm::enclave_witness_budget_deferral(*tx.tx().tx_hash())
            } else {
                outbe_evm::sponsored_block_budget_deferral(*tx.tx().tx_hash())
            });
        }
        self.inner
            .execute_transaction_with_commit_condition((env, tx), f)
    }

    fn execute_transaction_with_result_closure(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(&<Self::Executor as BlockExecutor>::TransactionResultWithState),
    ) -> Result<GasOutput, BlockExecutionError> {
        self.inner.execute_transaction_with_result_closure(tx, f)
    }

    fn set_state_hook(&mut self, hook: impl FnMut(StateUpdate) + Send + 'static) -> bool {
        self.inner.set_state_hook(hook)
    }

    fn finish_with_state_root(
        self,
        state_provider: impl reth_provider::StateProvider,
        state_root: impl FnOnce(
            &BlockExecutionOutput<<Self::Executor as BlockExecutor>::Receipt>,
        ) -> Result<
            Option<(B256, reth_trie::updates::TrieUpdates)>,
            BlockExecutionError,
        >,
    ) -> Result<BlockBuilderOutcome<Self::Primitives>, BlockExecutionError> {
        self.inner
            .finish_with_state_root(state_provider, state_root)
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        self.inner.executor_mut()
    }

    fn executor(&self) -> &Self::Executor {
        self.inner.executor()
    }

    fn into_executor(self) -> Self::Executor {
        self.inner.into_executor()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("node storage read failed: {0}")]
struct StageReadError(String);

/// Faults the actual EVM database read, not the selection/error-policy code.
#[derive(Debug)]
struct StageDatabase<DB> {
    inner: DB,
    fail_storage: bool,
}
impl<DB: Database> Database for StageDatabase<DB> {
    type Error = StageReadError;
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.inner
            .get_account(address)
            .map_err(|e| StageReadError(e.to_string()))
    }
    fn get_code_by_hash(&mut self, hash: &B256) -> Result<Bytecode, Self::Error> {
        self.inner
            .get_code_by_hash(hash)
            .map_err(|e| StageReadError(e.to_string()))
    }
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        if self.fail_storage && *address == address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC") {
            return Err(StageReadError("unavailable".into()));
        }
        self.inner
            .get_storage(address, key)
            .map_err(|e| StageReadError(e.to_string()))
    }
    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.inner
            .get_block_hash(number)
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
    cancel: CancelOnDrop,
    options: StageOptions,
) -> UserStageResult {
    if !options.finalize {
        return run_user_stage_inner(transactions, cancel, options);
    }
    let mut stage = None;
    let observed = observe_payload_build(|| {
        let mut result = run_user_stage_inner(transactions, cancel, options);
        let outcome = match std::mem::replace(&mut result.outcome, Ok(StageOutcome::Completed)) {
            Ok(StageOutcome::Completed) => Ok(BuildOutcome::Better {
                payload: result
                    .payload
                    .take()
                    .expect("completed fixture has a payload"),
                cached_reads: Default::default(),
            }),
            Ok(StageOutcome::Cancelled) => {
                result.outcome = Ok(StageOutcome::Cancelled);
                Ok(BuildOutcome::Cancelled)
            }
            Err(err) => Err(err),
        };
        stage = Some(result);
        outcome
    });
    let mut result = stage.expect("observed fixture completed");
    match observed {
        Ok(built) => result.payload = built.into_payload(),
        Err(err) => result.outcome = Err(err),
    }
    result
}

fn run_user_stage_inner(
    transactions: &[(u64, u64)],
    cancel: CancelOnDrop,
    options: StageOptions,
) -> UserStageResult {
    let (evm_config, provider) = narrow_user_stage_fixture();
    let chain_spec = provider.chain_spec();
    let block_gas_limit = 100_000;
    let parent = SealedHeader::seal_slow(OutbeHeader::new(alloy_consensus::Header {
        number: 1,
        gas_limit: block_gas_limit,
        timestamp: ACTIVE_PAYLOAD_BLOCK_TIMESTAMP - 1,
        // Zero burned fees keep this isolated finish fixture off the domain host.
        base_fee_per_gas: if options.finalize {
            Some(0)
        } else {
            Some(1_000_000_000)
        },
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
    // Prepare an isolated environment without requiring canonical Phase-1 metadata.
    // The builder below still executes block 2 and enforces its fixed reservation.
    let bootstrap_parent = SealedHeader::seal_slow(OutbeHeader::default());
    let mut env = preparation::prepare(
        &evm_config,
        &PayloadContext {
            parent: &bootstrap_parent,
            ..context
        },
    )
    .expect("execution attributes prepare")
    .env;
    env.execute_outbe_block_hooks = false;
    env.inner.gas_limit = block_gas_limit;
    if !options.code.is_empty() {
        provider.add_account(
            address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"),
            ExtendedAccount::new(0, U256::ZERO).with_bytecode(options.code.clone()),
        );
    }
    let stage_db = StageDatabase {
        inner: StateProviderDatabase::new(reth_storage_api::EvmStateProviderAdapter(&provider)),
        fail_storage: options.fail_storage,
    };
    let mut builder = evm_config
        .builder_for_next_block(stage_db, &parent, env)
        .expect("user-stage builder initializes");
    preparation::apply_pre_execution_changes(&mut builder).expect("pre-execution succeeds");
    let mut builder = DeferringBuilder {
        inner: builder,
        defer_next: options.defer_first,
        gas_deferral: options.gas_deferral,
        witness_deferral: options.witness_deferral,
    };
    let invalid = Arc::new(Mutex::new(Vec::new()));
    let mut candidates = Vec::new();
    let transactions = transactions
        .iter()
        .enumerate()
        .map(|(index, &(nonce, gas_limit))| {
            let tx: OutbeTxEnvelope = if options.blob {
                alloy_consensus::TxEip4844 {
                    chain_id: chain_spec.chain().id(),
                    nonce,
                    gas_limit,
                    blob_versioned_hashes: vec![B256::repeat_byte(1)],
                    ..Default::default()
                }
                .into_signed(Signature::test_signature())
                .into()
            } else {
                TxEip1559 {
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
                .into()
            };
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
            provider.add_account(
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
        block_gas_limit,
        builder.evm_env().block_base_fee(),
    )
    .expect("budgets initialize");
    if options.full_size {
        state
            .size_budget
            .record(outbe_primitives::consensus::OUTBE_MAX_BLOCK_SIZE);
    }
    let initial_size = state.size_budget.estimate(0).unwrap();
    let pool = TestPool::new();
    let outcome = UserTransactions {
        pool: &pool,
        best_txs: &mut best_txs,
        state: &mut state,
        cancel: &cancel,
        payload_id: PayloadId::new([0x09; 8]),
    }
    .execute(&mut builder);
    let receipts = builder
        .executor_mut()
        .receipts()
        .iter()
        .map(|r| r.success)
        .collect();
    let body_bytes = state.size_budget.estimate(0).unwrap() - initial_size;
    let gas_used = state.cumulative_gas_used;
    let fees = state.total_fees;
    let (payload, bundle_state) =
        if options.finalize && matches!(outcome, Ok(StageOutcome::Completed)) {
            let finished = finish_payload_with_state_root(
                builder,
                &provider,
                |_| {
                    options
                        .sparse_failure
                        .then_some(Err(PayloadBuilderError::MissingPayload))
                },
                PayloadId::new([0x09; 8]),
            )
            .expect("real EVM builder finishes");
            let bundle_state = finished.execution_state.clone();
            let payload = finalization::into_payload(finished, state, &context)
                .expect("production finalization succeeds");
            (Some(payload), bundle_state)
        } else {
            (None, builder.into_executor().into_state())
        };
    let included = candidates
        .into_iter()
        .filter(|&(sender, nonce, _)| {
            let post_nonce = bundle_state
                .account(&sender)
                .and_then(|account| account.info.as_ref().map(|info| info.nonce))
                .or_else(|| {
                    provider
                        .basic_account(&sender)
                        .expect("post-execution account is readable")
                        .map(|account| account.nonce)
                })
                .expect("funded user exists");
            post_nonce == nonce + 1
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
        gas_used,
        fees,
        body_bytes,
        rejected: invalid.len(),
        invalid,
        payload,
    }
}

#[test]
fn sponsored_budget_deferral_retains_candidate_and_continues_selection() {
    let result = run_user_stage_with_options(
        &[(0, 50_000), (4, 30_000), (0, 30_000)],
        Default::default(),
        StageOptions {
            account_nonce: Some(0),
            defer_first: true,
            ..Default::default()
        },
    );
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.receipts, vec![true]);
    assert_eq!(result.included_nonces, vec![0]);
    assert_eq!(result.gas_used, 21_000);
    assert_eq!(result.fees, U256::from(21_000u64 * 7));
    assert_eq!(
        result.rejected, 1,
        "only the unrelated invalid tx is evicted"
    );
    let InvalidPoolTransactionError::Other(error) = &result.invalid[0] else {
        panic!("expected preserved invalid transaction reason");
    };
    let preserved = error
        .as_any()
        .downcast_ref::<crate::payload_builder::transaction_error::PreservedInvalidUserTx>()
        .unwrap();
    assert!(matches!(
        preserved
            .reason
            .as_any()
            .downcast_ref::<evm2::registry::HandlerError>(),
        Some(evm2::registry::HandlerError::InvalidNonce {
            expected: 0,
            got: 4
        })
    ));
}

#[test]
fn executor_gas_capacity_deferral_retains_candidate_and_continues_selection() {
    let result = run_user_stage_with_options(
        &[(0, 50_000), (0, 30_000)],
        Default::default(),
        StageOptions {
            defer_first: true,
            gas_deferral: true,
            ..Default::default()
        },
    );
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.receipts, vec![true]);
    assert_eq!(result.gas_used, 21_000);
    assert_eq!(result.rejected, 0);
    assert_eq!(result.included_nonces, vec![0]);
}

#[test]
fn user_stage_accounts_only_included_transactions_and_skips_nonce_too_low() {
    let result = run_user_stage(&[(0, 30_000), (0, 30_000), (1, 30_000)], Default::default());
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.included_nonces, vec![0, 1]);
    assert_eq!(result.gas_used, 42_000);
    assert_eq!(result.fees, U256::from(42_000u64 * 7));
    assert!(result.body_bytes > 0);
    assert_eq!(result.rejected, 0, "nonce-too-low does not poison the pool");
}

#[test]
fn user_stage_cancellation_leaves_candidate_unexecuted_and_unaccounted() {
    let cancel = CancelOnDrop::default();
    drop(cancel.clone());
    let result = run_user_stage(&[(0, 30_000)], cancel);
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
    let result = run_user_stage(&[(0, 100_001)], cancel);
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert!(result.included_nonces.is_empty());
    assert_eq!(result.rejected, 1);
    assert_eq!(result.gas_used, 0);
    assert_eq!(result.fees, U256::ZERO);
    assert_eq!(result.body_bytes, 0);
}

#[test]
fn active_reservation_admits_exact_capacity_and_releases_unused_declared_gas() {
    let exact = run_user_stage(&[(0, 100_000)], Default::default());
    assert_eq!(exact.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(exact.receipts, vec![true]);
    assert_eq!(exact.gas_used, 21_000);
    assert_eq!(exact.rejected, 0);

    let reused = run_user_stage(
        &[(0, 100_000), (0, 79_000), (0, 58_001)],
        Default::default(),
    );
    assert_eq!(reused.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(reused.receipts, vec![true, true]);
    assert_eq!(reused.gas_used, 42_000);
    assert_eq!(reused.rejected, 1);
    assert_eq!(reused.fees, U256::from(42_000u64 * 7));
}

#[test]
fn user_revert_is_included_with_failed_receipt_and_consumed_nonce() {
    let result = run_user_stage_with_options(
        &[(0, 30_000), (0, 30_000)],
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
        preserved
            .reason
            .as_any()
            .downcast_ref::<evm2::registry::HandlerError>(),
        Some(evm2::registry::HandlerError::InvalidNonce {
            expected: 0,
            got: 4
        })
    ));
}

fn recorded_payload_metrics(
    recorder: &metrics_util::debugging::DebuggingRecorder,
) -> std::collections::BTreeMap<String, metrics_util::debugging::DebugValue> {
    recorder
        .snapshotter()
        .snapshot()
        .into_vec()
        .into_iter()
        .map(|(key, _, _, value)| {
            let mut name = key.key().name().to_owned();
            for label in key.key().labels() {
                assert_eq!(label.key(), "reason");
                name.push(':');
                name.push_str(label.value());
            }
            (name, value)
        })
        .collect()
}

#[test]
fn payload_selection_records_real_skip_reasons() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    let recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        let gas = run_user_stage(&[(0, 100_001)], Default::default());
        assert_eq!(gas.rejected, 1);
        let nonce = run_user_stage(&[(0, 30_000), (0, 30_000)], Default::default());
        assert_eq!(nonce.included_nonces, vec![0]);
        let invalid = run_user_stage_with_options(
            &[(4, 30_000)],
            Default::default(),
            StageOptions {
                account_nonce: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(invalid.rejected, 1);
    });
    let values = recorded_payload_metrics(&recorder);
    for reason in ["gas_limit", "nonce_too_low", "invalid_tx"] {
        assert_eq!(
            values.get(&format!("outbe_evm_payload_tx_skipped_total:{reason}")),
            Some(&DebugValue::Counter(1))
        );
    }
    assert!(!values
        .keys()
        .any(|key| key.starts_with("outbe_evm_payload_build_stop_total")));
}

#[test]
fn payload_stage_finish_records_success_metrics_and_exact_rlp_size() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use reth_payload_primitives::BuiltPayload as _;
    let recorder = DebuggingRecorder::new();
    let result = metrics::with_local_recorder(&recorder, || {
        run_user_stage_with_options(
            &[(0, 30_000)],
            Default::default(),
            StageOptions {
                finalize: true,
                ..Default::default()
            },
        )
    });
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.receipts, vec![true]);
    assert_eq!(result.included_nonces, vec![0]);
    let rlp_size = result.payload.unwrap().block().rlp_length();
    assert!(rlp_size > 0);
    let values = recorded_payload_metrics(&recorder);
    assert_eq!(
        values["outbe_evm_payload_rlp_size_bytes"],
        DebugValue::Gauge((rlp_size as f64).into())
    );
    assert_eq!(
        values["outbe_evm_payload_build_stop_total:candidates_exhausted"],
        DebugValue::Counter(1)
    );
    for name in [
        "outbe_evm_payload_build_duration_ms",
        "outbe_evm_payload_state_root_duration_ms",
    ] {
        let DebugValue::Histogram(samples) = &values[name] else {
            panic!("expected duration histogram");
        };
        assert_eq!(samples.len(), 1);
        assert!(samples[0].0.is_finite() && samples[0].0 >= 0.0);
    }
    assert!(!values.contains_key("outbe_evm_payload_build_stop_total:aborted_not_better"));
}

#[test]
fn payload_stage_cancellation_records_only_cancelled() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    let recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        let cancel = CancelOnDrop::default();
        drop(cancel.clone());
        let result = run_user_stage_with_options(
            &[(0, 30_000)],
            cancel,
            StageOptions {
                finalize: true,
                ..Default::default()
            },
        );
        assert_eq!(result.outcome.unwrap(), StageOutcome::Cancelled);
        assert!(result.receipts.is_empty());
        assert_eq!(result.body_bytes, 0);
        assert!(result.payload.is_none());
    });
    let values = recorded_payload_metrics(&recorder);
    assert_eq!(values.len(), 1);
    assert_eq!(
        values["outbe_evm_payload_build_stop_total:cancelled"],
        DebugValue::Counter(1)
    );
}

#[test]
fn payload_skip_reason_emitters_have_eight_closed_labels_and_fallback_counter() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use PayloadTxSkipReason::*;
    let recorder = DebuggingRecorder::new();
    let reasons = [
        (GasLimit, "gas_limit"),
        (RlpSizeLimit, "rlp_size_limit"),
        (BlobRejected, "blob_rejected"),
        (GasAboveAvailable, "gas_above_available"),
        (NonceTooLow, "nonce_too_low"),
        (SponsoredBudgetDeferred, "sponsored_budget_deferred"),
        (
            EnclaveWitnessBudgetDeferred,
            "enclave_witness_budget_deferred",
        ),
        (InvalidTx, "invalid_tx"),
    ];
    metrics::with_local_recorder(&recorder, || {
        for (reason, label) in reasons {
            assert_eq!(reason.as_label(), label);
            reason.record();
        }
        record_payload_sparse_trie_fallback();
    });
    let values = recorded_payload_metrics(&recorder);
    assert_eq!(values.len(), 9);
    for (_, label) in reasons {
        assert_eq!(
            values[&format!("outbe_evm_payload_tx_skipped_total:{label}")],
            DebugValue::Counter(1)
        );
    }
    assert_eq!(
        values["outbe_evm_payload_sparse_trie_fallback_total"],
        DebugValue::Counter(1)
    );
}

#[test]
fn payload_terminal_outcomes_are_mutually_exclusive_and_errors_emit_nothing() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    let outcomes = [
        (
            Ok(BuildOutcome::Better {
                payload: (),
                cached_reads: Default::default(),
            }),
            Some("candidates_exhausted"),
        ),
        (
            Ok(BuildOutcome::Aborted {
                fees: U256::ZERO,
                cached_reads: Default::default(),
            }),
            Some("aborted_not_better"),
        ),
        (Ok(BuildOutcome::Cancelled), Some("cancelled")),
        (Err(PayloadBuilderError::MissingPayload), None),
    ];
    for (outcome, reason) in outcomes {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            record_payload_build_outcome(&outcome, std::time::Duration::from_micros(1500))
        });
        let values = recorded_payload_metrics(&recorder);
        let Some(reason) = reason else {
            assert!(values.is_empty());
            continue;
        };
        let stops: Vec<_> = values
            .iter()
            .filter(|(key, _)| key.starts_with("outbe_evm_payload_build_stop_total:"))
            .collect();
        assert_eq!(stops.len(), 1);
        assert_eq!(
            stops[0].0,
            &format!("outbe_evm_payload_build_stop_total:{reason}")
        );
        assert_eq!(stops[0].1, &DebugValue::Counter(1));
        assert_eq!(
            values.contains_key("outbe_evm_payload_build_duration_ms"),
            reason == "candidates_exhausted"
        );
        if reason == "candidates_exhausted" {
            assert_eq!(
                values["outbe_evm_payload_build_duration_ms"],
                DebugValue::Histogram(vec![1.5.into()])
            );
        }
    }
}

#[test]
fn payload_selection_records_size_blob_and_executor_deferrals() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    let cases = [
        (
            StageOptions {
                full_size: true,
                ..Default::default()
            },
            "rlp_size_limit",
            1,
        ),
        (
            StageOptions {
                blob: true,
                ..Default::default()
            },
            "blob_rejected",
            1,
        ),
        (
            StageOptions {
                defer_first: true,
                gas_deferral: true,
                ..Default::default()
            },
            "gas_above_available",
            0,
        ),
        (
            StageOptions {
                defer_first: true,
                ..Default::default()
            },
            "sponsored_budget_deferred",
            0,
        ),
        (
            StageOptions {
                defer_first: true,
                witness_deferral: true,
                ..Default::default()
            },
            "enclave_witness_budget_deferred",
            0,
        ),
    ];
    for (options, label, rejected) in cases {
        let recorder = DebuggingRecorder::new();
        let result = metrics::with_local_recorder(&recorder, || {
            run_user_stage_with_options(&[(0, 30_000)], Default::default(), options)
        });
        assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
        assert!(result.receipts.is_empty());
        assert_eq!(result.rejected, rejected);
        assert_eq!(result.gas_used, 0);
        assert_eq!(result.body_bytes, 0);
        let values = recorded_payload_metrics(&recorder);
        assert_eq!(
            values[&format!("outbe_evm_payload_tx_skipped_total:{label}")],
            DebugValue::Counter(1)
        );
    }
}

#[test]
fn payload_failed_state_root_stage_records_no_duration() {
    let recorder = metrics_util::debugging::DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        let result: Result<(), _> =
            measure_payload_state_root(|| Err(PayloadBuilderError::MissingPayload));
        assert!(result.is_err());
    });
    assert!(recorded_payload_metrics(&recorder).is_empty());
}

#[test]
fn payload_failed_sparse_root_falls_back_to_real_finish_and_finalization() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use reth_payload_primitives::BuiltPayload as _;
    let recorder = DebuggingRecorder::new();
    let result = metrics::with_local_recorder(&recorder, || {
        run_user_stage_with_options(
            &[(0, 30_000)],
            Default::default(),
            StageOptions {
                finalize: true,
                sparse_failure: true,
                ..Default::default()
            },
        )
    });
    assert_eq!(result.outcome.unwrap(), StageOutcome::Completed);
    assert_eq!(result.receipts, vec![true]);
    assert_eq!(result.included_nonces, vec![0]);
    assert_eq!(result.gas_used, 21_000);
    let rlp_size = result.payload.unwrap().block().rlp_length();
    assert!(rlp_size > 0);
    let values = recorded_payload_metrics(&recorder);
    assert_eq!(
        values["outbe_evm_payload_sparse_trie_fallback_total"],
        DebugValue::Counter(1)
    );
    assert_eq!(
        values["outbe_evm_payload_rlp_size_bytes"],
        DebugValue::Gauge((rlp_size as f64).into())
    );
    assert_eq!(
        values["outbe_evm_payload_build_stop_total:candidates_exhausted"],
        DebugValue::Counter(1)
    );
    let DebugValue::Histogram(samples) = &values["outbe_evm_payload_state_root_duration_ms"] else {
        panic!("expected successful fallback duration");
    };
    assert_eq!(samples.len(), 1);
}
