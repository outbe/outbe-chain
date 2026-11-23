use super::*;
use alloy_eips::eip7928::{BlockAccessIndex, BlockAccessList};
use alloy_primitives::map::{AddressSet, B256HashMap};

/// Outbe block executor.
///
/// Wraps the standard [`EthBlockExecutor`] and routes Outbe system transactions
/// through the same ordered transaction/receipt path as user transactions.
/// `apply_pre_execution_changes()` performs pre-block setup. On blocks with
/// Phase 1, it also verifies, executes, and commits the Phase 1
/// (`CertifiedParentAccounting`) system tx. Then it runs the begin-block storage
/// hooks. In the main loop, the executor validates `body[0]` against that witness
/// and does not execute it again. The other begin-zone phases execute when their
/// reserved-address body transaction reaches the loop.
pub struct OutbeBlockExecutor<'a> {
    /// Inner Ethereum execution strategy.
    pub inner: EthBlockExecutor<'a, OutbeEvmTypes, &'a RethReceiptBuilder>,
    /// Canonical Outbe chain specification.
    pub chain_spec: Arc<ChainSpec<OutbeHeader>>,
    /// Raw block withdrawals retained for Outbe native withdrawal validation
    /// (inner `EthBlockExecutor` receives `withdrawals: None` so Ethereum's
    /// Gwei-to-Wei conversion never runs on native withdrawals).
    pub withdrawals: Option<Vec<Withdrawal>>,
    /// Immutable chain identity sourced from the executor's canonical ChainSpec.
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub genesis_timestamp: u64,
    /// Optional bridge to the consensus layer for finalization data.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Header-carried consensus artifact bytes (`extra_data`) used by begin-zone phases.
    pub block_extra_data: Bytes,
    /// Canonical final header `extra_data` bytes.
    pub final_extra_data: Bytes,
    /// Historical header artifact reader used for finalized-block settlement.
    pub accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    /// Whether this executor is validating an already-built block and must
    /// compare the header-carried execution summary to local execution output.
    pub validate_execution_summary: bool,
    /// Hash of the block being validated, when execution is for an existing block.
    pub block_hash: Option<B256>,
    /// State root committed by the validated block, cached with its execution summary.
    block_state_root: Option<B256>,
    /// Hash of this block's parent header.
    pub parent_hash: B256,
    /// Priority/coinbase fees collected by user transactions in this block.
    pub current_block_validator_fees: U256,
    /// Base fees and EIP-4844 blob fees burned by paid transactions in this block.
    pub current_block_burned_fees: U256,
    /// Whether `settle_block_burned_fees` has already flushed this block's burned fees.
    pub burned_fees_settled: bool,
    /// Refund-adjusted gas consumed by begin-zone system transactions under the
    /// Outbe-only execution lane (`SYSTEM_TX_ARTIFACT_GAS_LIMIT`, 10B gas).
    pub system_tx_execution_gas: u64,
    /// Gross begin-zone gas before refunds.
    pub system_tx_gross_gas: u64,
    /// Validator-mode signer used by proposer path to sign system-tx artifacts.
    pub evm_signer: Option<SharedOutbeEvmSigner>,
    pub expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
    /// Standard Ethereum post-execution output used to assemble the final result.
    pub ethereum_post_execution_requests: Option<Requests>,
    pub system_layout_error: Option<crate::errors::SystemTxExpectationError>,
    pub parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub proposer_evm_address: Option<Address>,
    pub execute_outbe_block_hooks: bool,
    /// Runtime-injected protocol step registry.
    pub block_order: crate::system_tx::BlockOrder,
    /// Signature hash committed in pre-execution; cleared only after FSM admission.
    pub preexecuted_phase1_tx_hash: Option<B256>,
    /// Proposer-side prebuilt Phase 1 body[0] tx.
    pub prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    /// Optional accounted-parent artifact hint from the payload builder.
    pub parent_artifact_hint: Option<AccountedParentArtifact>,
    /// Canonical VRF proof hash captured from the verified parent certificate.
    pub verified_phase1_vrf_proof_hash: Option<B256>,
    /// Proposer-only one-time Phase 3b `TeeBootstrap` payload.
    pub pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Explicit block execution state machine.
    pub fsm: crate::system_tx::BlockExecutionFsm,
    /// Number of zero-fee soft-failure receipts emitted in THIS block.
    pub zero_fee_soft_failures: u32,
    /// Per-hook soft-failure counts, reset with each block executor.
    pub(super) zero_fee_hook_soft_failures: std::collections::BTreeMap<u8, u32>,
    /// Sum of included sponsored and bootstrap gas limits, reset per block.
    pub sponsored_gas_used: u64,
    /// Block-scoped consensus enclave witness state.
    pub witness_state: Option<Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    /// Zero-based index of the next committed transaction in `body.transactions`.
    pub body_tx_index: u32,
    /// Cached compressed `OART 0x0A` payload on the proposer path.
    pub frozen_enclave_witness: Option<Bytes>,
    /// End-zone transactions built by the proposer. The block builder appends them.
    pub staged_end_block_txs: Vec<Recovered<TransactionSigned>>,
    /// Decoded `block_extra_data` cached once at block open.
    pub decoded_block_artifacts: Option<OutbeBlockArtifacts>,
    /// Phase-1 (`CertifiedParentAccounting`) input and parent execution summary cached once.
    pub cached_phase1_input: Option<(SystemTx, AccountedParentArtifact)>,
    /// Resolved begin-zone system transactions and visible gas plan computed once per block.
    pub resolved_begin_zone: Option<Vec<ResolvedBeginSystemTx>>,
    /// Pre-verified signer indices per `LateFinalizeCredits` batch.
    pub verified_late_credit_signers: Option<Arc<Vec<Vec<usize>>>>,
    /// Domain host captured when this executor is built.
    pub runtime_host: Arc<dyn crate::EvmRuntimeHost>,
}

impl<'a> OutbeBlockExecutor<'a> {
    pub fn new(
        inner: EthBlockExecutor<'a, OutbeEvmTypes, &'a RethReceiptBuilder>,
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        withdrawals: Option<Vec<Withdrawal>>,
        inputs: BlockExecutorInputs,
    ) -> Self {
        let BlockExecutorInputs {
            identity:
                BlockExecutionIdentity {
                    block_extra_data,
                    validate_execution_summary,
                    block_hash,
                    block_state_root,
                    parent_hash,
                },
            system_plan:
                BlockSystemPlan {
                    expected_begin_system_txs,
                    system_layout_error,
                    proposer_evm_address,
                    execute_outbe_block_hooks,
                    prebuilt_phase1_tx,
                    pending_tee_bootstrap,
                },
            parent_accounting:
                ParentAccountingInputs {
                    accounted_parent_artifact_provider,
                    parent_consensus_metadata,
                    parent_artifact_hint,
                },
            dependencies:
                BlockExecutionDependencies {
                    bridge,
                    evm_signer,
                    runtime_host,
                },
        } = inputs;
        let block_order = runtime_host.block_order();
        let chain_id = chain_spec.chain().id();
        let genesis_hash = chain_spec.genesis_hash();
        let genesis_timestamp = chain_spec.genesis_timestamp();
        Self {
            inner,
            chain_spec,
            withdrawals,
            chain_id,
            genesis_hash,
            genesis_timestamp,
            bridge,
            final_extra_data: block_extra_data.clone(),
            block_extra_data,
            accounted_parent_artifact_provider,
            validate_execution_summary,
            block_hash,
            block_state_root,
            parent_hash,
            current_block_validator_fees: U256::ZERO,
            current_block_burned_fees: U256::ZERO,
            burned_fees_settled: false,
            system_tx_execution_gas: 0,
            system_tx_gross_gas: 0,
            evm_signer,
            expected_begin_system_txs,
            ethereum_post_execution_requests: None,
            system_layout_error,
            parent_consensus_metadata,
            proposer_evm_address,
            execute_outbe_block_hooks,
            block_order,
            preexecuted_phase1_tx_hash: None,
            prebuilt_phase1_tx,
            parent_artifact_hint,
            verified_phase1_vrf_proof_hash: None,
            pending_tee_bootstrap,
            fsm: crate::system_tx::BlockExecutionFsm::for_mid_block(&block_order),
            zero_fee_soft_failures: 0,
            zero_fee_hook_soft_failures: std::collections::BTreeMap::new(),
            sponsored_gas_used: 0,
            witness_state: None,
            body_tx_index: 0,
            frozen_enclave_witness: None,
            staged_end_block_txs: Vec::new(),
            decoded_block_artifacts: None,
            cached_phase1_input: None,
            resolved_begin_zone: None,
            verified_late_credit_signers: None,
            runtime_host,
        }
    }

    #[inline]
    pub(crate) fn block_number(&self) -> u64 {
        self.inner.evm().block().number.saturating_to::<u64>()
    }

    #[inline]
    pub(crate) fn block_timestamp_u256(&self) -> U256 {
        self.inner.evm().block().timestamp
    }

    #[inline]
    pub(crate) fn block_beneficiary(&self) -> Address {
        self.inner.evm().block().beneficiary
    }

    #[inline]
    pub(crate) fn block_gas_limit(&self) -> u64 {
        self.inner.evm().block().gas_limit.saturating_to::<u64>()
    }

    #[inline]
    pub(crate) fn block_prevrandao(&self) -> Option<B256> {
        Some(B256::from(self.inner.evm().block().prevrandao))
    }

    #[inline]
    pub(crate) fn block_basefee(&self) -> u128 {
        self.inner.evm().block().basefee.saturating_to::<u128>()
    }

    #[inline]
    pub(crate) fn block_blob_gasprice(&self) -> u128 {
        self.inner
            .evm()
            .block()
            .blob_basefee
            .saturating_to::<u128>()
    }

    /// Propagates a [`BlockStateAccumulator`] produced by [`DirectStorageProvider::flush`] into
    /// `self.inner.inner.block_state` AND any attached `StateUpdate` hook (`OnStateHook` /
    /// parallel state root) via `self.inner.commit_pending_state(&pending)`.
    pub(crate) fn commit_block_accumulator(
        &mut self,
        changes: &BlockStateAccumulator,
    ) -> Result<(), BlockExecutionError> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut pending = PendingState::default();
        let codes: B256HashMap<evm2::bytecode::Bytecode> = changes
            .code()
            .map(|(hash, code)| (*hash, code.clone()))
            .collect();
        let mut inserted = AddressSet::default();

        for (addr, tracked) in changes.accounts() {
            let mut current = tracked.current.clone();
            if let Some(info) = current.as_mut() {
                if info.code.is_none() {
                    if let Some(code) = codes.get(&info.code_hash) {
                        info.code = Some(code.clone());
                    }
                }
            }
            pending.insert_account(addr, tracked.original.clone(), current);
            inserted.insert(addr);
        }

        for (key, tracked) in changes.storage() {
            let addr = key.address();
            if !inserted.contains(&addr) {
                let info = self
                    .inner
                    .evm_mut()
                    .overlay_db_mut()
                    .get_account(&addr)
                    .map_err(BlockExecutionError::other)?;
                pending.insert_account(addr, info.clone(), info);
                inserted.insert(addr);
            }
            pending.insert_storage(addr, key.key(), tracked.original, tracked.current);
        }

        self.inner.commit_pending_state(&pending);
        Ok(())
    }

    pub fn current_execution_summary(&self) -> ExecutionSummaryArtifact {
        ExecutionSummaryArtifact {
            validator_fee_sum: self.current_block_validator_fees,
        }
    }

    /// Canonical final header `extra_data` bytes used by `finish()` for
    /// execution-summary validation and bridge recording.
    pub fn final_extra_data(&self) -> &Bytes {
        &self.final_extra_data
    }

    pub fn set_final_extra_data(&mut self, bytes: Bytes) {
        self.final_extra_data = bytes;
    }

    /// Pre-encodes the final execution-produced header fields.
    /// Settles burned fees before encoding on the parallel-root path.
    pub fn prepare_final_header_artifacts(
        &mut self,
        timestamp_millis_part: u64,
    ) -> Result<(), BlockExecutionError> {
        self.settle_block_burned_fees()?;
        let enclave_witness = self.final_enclave_witness_bytes()?;
        self.final_extra_data = crate::builder::encode_final_header_artifacts(
            self.final_extra_data.as_ref(),
            self.current_execution_summary(),
            timestamp_millis_part,
            enclave_witness,
        )?;
        Ok(())
    }
}

impl<'a> BlockExecutor for OutbeBlockExecutor<'a> {
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = OutbeEvm<'a>;
    type TransactionResultWithState =
        EthTransactionResultWithState<OutbeEvmTypes, alloy_consensus::TxType>;
    type BlockAccessList = evm2::evm::Bal;

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn set_state_hook(&mut self, hook: impl FnMut(StateUpdate) + Send + 'static) -> bool {
        self.inner.set_state_hook(hook)
    }

    fn convert_block_access_list(
        block_access_list: &BlockAccessList,
    ) -> Result<Self::BlockAccessList, BlockExecutionError> {
        evm2::evm::Bal::try_from(block_access_list.as_slice()).map_err(BlockExecutionError::other)
    }

    fn set_block_access_list(&mut self, block_access_list: Arc<Self::BlockAccessList>) {
        self.inner.set_block_access_list(block_access_list);
    }

    fn set_block_access_index(&mut self, index: BlockAccessIndex) {
        self.inner.set_block_access_index(index);
    }

    fn enable_block_access_list_builder(&mut self) {
        self.inner.enable_block_access_list_builder();
    }

    fn take_block_access_list(&mut self) -> Option<BlockAccessList> {
        self.inner.take_block_access_list()
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.apply_outbe_pre_execution()
    }

    fn into_state(self) -> reth_evm::BundleState {
        self.inner.into_state()
    }

    fn validate_transaction_gas_limit(
        &mut self,
        transaction_gas_limit: u64,
    ) -> Result<(), BlockExecutionError> {
        self.inner
            .validate_transaction_gas_limit(transaction_gas_limit)
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutorTx<Self>,
    ) -> Result<Self::TransactionResultWithState, BlockExecutionError> {
        crate::executor::witness::clear_detached_trace_witness();
        outbe_primitives::time::timestamp_secs(self.block_timestamp_u256())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        let (tx_env, recovered) = tx.into_parts();
        if is_reserved_system_tx(recovered.tx()) {
            return Err(
                crate::errors::ExecutorValidationError::ReservedSystemTxWithoutCommit.into(),
            );
        }
        if self.has_active_consensus_witness_scope() {
            return Err(
                crate::errors::ExecutorValidationError::WitnessRequiresCommitCondition.into(),
            );
        }
        if self.witness_state.is_none() {
            self.init_rpc_direct_witness_state();
            self.inner
                .evm_mut()
                .ext_mut()
                .set_attached_witness_state(self.witness_state.clone());
        }
        let _witness_scope = self.prepare_tx_witness_scope(false)?;
        self.inner
            .execute_transaction_without_commit((tx_env, recovered))
            .map_err(preserve_cancelled_execution)
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self>,
        f: impl FnOnce(&TxResult<OutbeEvmTypes>, TxStateChanges<'_>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        crate::executor::witness::clear_detached_trace_witness();
        outbe_primitives::time::timestamp_secs(self.block_timestamp_u256())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        let (tx_env, recovered) = tx.into_parts();
        let tx_hash = *recovered.tx().tx_hash();
        if is_end_block_system_tx(recovered.tx()) {
            return self
                .execute_end_block_system_tx(recovered.tx(), *recovered.signer())
                .map_err(preserve_cancelled_execution);
        }
        let is_system_tx = is_reserved_system_tx(recovered.tx());
        if is_system_tx && self.is_preexecuted_phase1_witness(recovered.tx()) {
            let outcome = self.execute_reserved_system_tx(recovered, f);
            if outcome.is_ok() {
                self.advance_preexecuted_phase1_witness_index()?;
            }
            return outcome.map_err(preserve_cancelled_execution);
        }
        let is_user_candidate = !is_system_tx;
        if is_user_candidate
            && self.execute_outbe_block_hooks
            && self.decoded_block_artifacts.is_some()
        {
            self.fsm
                .on_user_tx()
                .map_err(crate::errors::ExecutorValidationError::from)?;
        }
        let witness_scope = self.prepare_tx_witness_scope(is_user_candidate)?;
        let witness_state = self.witness_state.clone();
        let body_tx_index = self.body_tx_index;
        let base_extra_data = self.block_extra_data.clone();
        let wrapped_f =
            |result: &TxResult<OutbeEvmTypes>, changes: TxStateChanges<'_>| -> CommitChanges {
                if !Self::check_witness_commit_allowed_with(
                    witness_state.as_ref(),
                    body_tx_index,
                    base_extra_data.as_ref(),
                    is_user_candidate,
                ) {
                    return CommitChanges::No;
                }
                let decision = f(result, changes);
                if !decision.should_commit() {
                    clear_uncommitted_witness_on_decline(witness_state.as_ref());
                }
                decision
            };
        let outcome = if is_system_tx {
            self.execute_reserved_system_tx(recovered, wrapped_f)
        } else {
            self.route_user_transaction(tx_env, recovered, wrapped_f)
        };
        drop(witness_scope);
        let committed = matches!(outcome, Ok(Some(_)));
        self.finish_tx_witness(tx_hash, committed)?;
        outcome.map_err(preserve_cancelled_execution)
    }

    fn commit_transaction(
        &mut self,
        output: Self::TransactionResultWithState,
    ) -> Result<GasOutput, BlockExecutionError> {
        self.inner.commit_transaction(output)
    }

    fn finish_with_block_access_list(
        self,
    ) -> Result<(BlockExecutionOutput<Self::Receipt>, Option<BlockAccessList>), BlockExecutionError>
    {
        let (output, bal) = self.finish_with_prepared_block_access_list()?;
        Ok((output, bal.map(Into::into)))
    }

    fn finish_with_prepared_block_access_list(
        mut self,
    ) -> Result<(BlockExecutionOutput<Self::Receipt>, Option<evm2::evm::Bal>), BlockExecutionError>
    {
        crate::executor::witness::clear_detached_trace_witness();
        let block_timestamp = outbe_primitives::time::timestamp_secs(self.block_timestamp_u256())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        if self.execute_outbe_block_hooks {
            self.fsm
                .finish()
                .map_err(crate::errors::ExecutorValidationError::from)?;
        }
        let current_summary = self.current_execution_summary();
        let block_number = self.block_number();
        let block_artifacts =
            artifacts::validate_finish_artifacts(&self, block_number, current_summary)?;

        self.apply_outbe_ethereum_post_execution()?;
        let requests = self
            .ethereum_post_execution_requests
            .take()
            .ok_or(crate::errors::ExecutorValidationError::MissingPostExecutionOutput)?;
        let gas_used = self.accounted_block_gas();
        let block_gas_limit = self.block_gas_limit();
        if gas_used > block_gas_limit {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: gas_used,
                    block_available_gas: block_gas_limit,
                }
                .into(),
            );
        }
        let receipts = std::mem::take(&mut self.inner.inner.receipts);
        let blob_gas_used = self.inner.inner.blob_gas_used;
        let bal = self.inner.inner.evm.state_mut().take_bal_builder();
        let state = self.inner.into_state();
        let result = BlockExecutionResult {
            receipts,
            requests,
            gas_used,
            blob_gas_used,
        };
        if let (Some(bridge), Some(block_hash), Some(summary)) = (
            self.bridge.as_ref(),
            self.block_hash,
            block_artifacts.execution_summary,
        ) {
            if let Some(state_root) = self.block_state_root {
                bridge.record_execution_summary_with_state_root(
                    block_number,
                    block_hash,
                    summary,
                    block_timestamp,
                    state_root,
                );
            } else {
                bridge.record_execution_summary(block_number, block_hash, summary, block_timestamp);
            }
        }

        Ok((BlockExecutionOutput::new(result, state), bal))
    }
}

fn is_end_block_system_tx(tx: &TransactionSigned) -> bool {
    is_reserved_system_tx(tx)
        && matches!(SystemTx::decode(tx.input()), Ok(SystemTx::EndBlock { .. }))
}

fn clear_uncommitted_witness_on_decline(
    witness_state: Option<&Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
) {
    if let Some(state) = witness_state {
        if let Ok(mut guard) = state.lock() {
            guard.staged.clear();
            guard.replay_cursor = 0;
        }
    }
}

use crate::precompiles::preserve_cancelled_execution;

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use evm2::{registry::HandlerError, AnyError};
    use outbe_primitives::projection::{
        ExecutionReadBudget, ExecutionReadCancelled, NodeLocalExecutionAbort,
    };

    #[test]
    fn raw_execution_abort_is_internal_and_keeps_its_typed_cause() {
        let budget = ExecutionReadBudget::new();
        budget.cancel();
        let evm_error = HandlerError::Fatal(AnyError::new(ExecutionReadCancelled {
            budget: budget.clone(),
        }));
        let error = BlockExecutionError::Internal(InternalBlockExecutionError::EVM {
            hash: B256::ZERO,
            error: AnyError::new(evm_error),
        });
        let error = preserve_cancelled_execution(error);
        assert!(matches!(error, BlockExecutionError::Internal(_)));
        let cancelled = ExecutionReadCancelled::find(&error).unwrap();
        assert!(cancelled.budget.same_request(&budget));
    }

    #[test]
    fn node_local_execution_abort_is_preserved_across_evm_error() {
        let evm_error = HandlerError::Fatal(AnyError::new(NodeLocalExecutionAbort::new(
            "TEE sidecar down",
        )));
        let error = BlockExecutionError::Internal(InternalBlockExecutionError::EVM {
            hash: B256::ZERO,
            error: AnyError::new(evm_error),
        });
        let error = preserve_cancelled_execution(error);
        assert!(matches!(error, BlockExecutionError::Internal(_)));
        let abort = NodeLocalExecutionAbort::find(&error).unwrap();
        assert_eq!(abort.reason, "TEE sidecar down");
    }
}
