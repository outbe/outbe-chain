use super::*;

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
pub struct OutbeBlockExecutor<'a, Evm> {
    /// Inner Ethereum execution strategy.
    pub inner: EthBlockExecutor<'a, Evm, &'a Arc<ChainSpec<OutbeHeader>>, &'a RethReceiptBuilder>,
    /// Immutable chain identity sourced from the executor's canonical ChainSpec.
    pub(super) genesis_hash: B256,
    /// Optional bridge to the consensus layer for finalization data.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Header-carried consensus artifact bytes (`extra_data`) used by begin-zone phases.
    pub(super) block_extra_data: Bytes,
    /// Canonical final header `extra_data` bytes. On the verifier path this is
    /// initialized from the sealed block header. On the proposer path the block
    /// builder overwrites it after it injects the execution summary and timestamp
    /// millis, but before `finish()`.
    pub(super) final_extra_data: Bytes,
    /// Historical header artifact reader used for finalized-block settlement.
    pub(super) accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    /// Whether this executor is validating an already-built block and must
    /// compare the header-carried execution summary to local execution output.
    pub(super) validate_execution_summary: bool,
    /// Hash of the block being validated, when execution is for an existing block.
    pub(super) block_hash: Option<B256>,
    /// State root committed by the validated block, cached with its execution summary.
    block_state_root: Option<B256>,
    /// Hash of this block's parent header.
    pub(super) parent_hash: B256,
    /// Priority/coinbase fees collected by user transactions in this block.
    pub(super) current_block_validator_fees: U256,
    /// Internal gas consumed by begin-zone system transactions under the
    /// Outbe-only execution lane (`SYSTEM_TX_ARTIFACT_GAS_LIMIT`, 10B gas). The
    /// Ethereum-visible block counters use each system tx envelope's visible
    /// intrinsic gas instead.
    pub(super) system_tx_execution_gas: u64,
    /// Validator-mode signer used by proposer path to sign system-tx artifacts.
    pub(super) evm_signer: Option<SharedOutbeEvmSigner>,
    pub(super) expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
    /// Standard Ethereum post-execution output used to assemble the final result.
    pub(super) ethereum_post_execution_requests: Option<Requests>,
    pub(super) system_layout_error: Option<crate::errors::SystemTxExpectationError>,
    pub(super) parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub(super) proposer_evm_address: Option<Address>,
    pub(super) execute_outbe_block_hooks: bool,
    /// Cursor that drives begin-zone phase routing inside
    /// `execute_transaction_with_commit_condition` instead of
    /// `self.inner.receipts.len()`. The executor sets it to the per-block
    /// initial value when it enters `apply_pre_execution_changes`. The executor
    /// advances it once per consumed begin-zone system tx.
    pub(super) system_tx_phase_cursor: crate::system_tx::SystemTxPhase,
    /// Proposer-side prebuilt Phase 1 body[0] tx. The payload builder sets it
    /// before `apply_pre_execution_changes`. `apply_phase1_commit_in_preexec`
    /// consumes it as the canonical witness whose `signature_hash` is cached in
    /// the phase cursor. It is `None` on the validator path (the witness comes
    /// from `expected_begin_system_txs.first()`) and for
    /// `block_number <= GENESIS_BOOTSTRAP_BLOCK_NUMBER`.
    pub(super) prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    /// Optional accounted-parent artifact hint from the payload builder.
    /// [`Self::accounted_parent_artifact_for_metadata`] consumes it when the
    /// [`AccountedParentArtifactProvider`] returns `None`. That function accepts
    /// the hint only if the metadata's `(finalized_block_number, finalized_block_hash)`
    /// matches `(block_number - 1, self.parent_hash)`.
    pub(super) parent_artifact_hint: Option<AccountedParentArtifact>,
    /// Canonical VRF proof hash that `verify_phase1_in_preexec` captures from
    /// the verified parent certificate
    /// (`outbe_consensus::proof::VerifiedProof::vrf_proof_hash`).
    /// `apply_phase1_commit_in_preexec` and the main-tx-loop Phase 1 path
    /// consume it to populate `PreloadedSystemTxContext.canonical_vrf_proof_hash`.
    /// The V3 Rewards fingerprint binds that field. It is `None` until the
    /// preflight runs. It remains `None` for skip paths (block 0 / 1, test opt-out).
    pub(super) verified_phase1_vrf_proof_hash: Option<B256>,
    /// Proposer-only one-time Phase 3b `TeeBootstrap` payload. When `Some` on the
    /// proposer path, `begin_block_system_tx_inputs` injects the bootstrap system
    /// tx after `BoundaryOutcome`. It does this identically to
    /// `build_begin_system_txs`, so the body that the proposer signs matches the
    /// inputs that the executor expects. It is `None` on the validator path (the
    /// body carries it via `expected_begin_system_txs`) and until the tribute-DKG
    /// bootstrap producer supplies a payload.
    pub(super) pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Whitelisted pre-exec hook logs published through the mandatory
    /// `HookEvents` system tx receipt at the end of the begin zone.
    pub(super) whitelisted_hook_event_logs: Vec<Log>,
    /// Number of zero-fee soft-failure receipts emitted in THIS
    /// block. Bounds block-stuffing by zero-cost 21k soft-failures (see
    /// [`Self::record_zero_fee_soft_failure`]). The executor is constructed
    /// fresh per block, so this resets per block. It is identical on the
    /// proposer (build) and validator (re-execution) paths.
    pub(super) zero_fee_soft_failures: u32,
    /// Sum of included sponsored and bootstrap gas limits, reset per block.
    pub(super) sponsored_gas_used: u64,
    /// Block-scoped consensus enclave witness state (`ProposerRecord`, `VerifierWithEnclave`,
    /// `VerifierReplay`, or `RpcDirect`).
    pub(super) witness_state: Option<Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    /// Zero-based index of the next committed transaction in `body.transactions`.
    pub(super) body_tx_index: u32,
    /// Cached compressed `OART 0x0A` payload on the proposer path so `prepare_final_header_artifacts`
    /// and `finalize_header_artifacts` never re-compress the witness tape.
    pub(super) frozen_enclave_witness: Option<Bytes>,
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub(crate) fn new(
        inner: EthBlockExecutor<'a, Evm, &'a Arc<ChainSpec<OutbeHeader>>, &'a RethReceiptBuilder>,
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
            dependencies: BlockExecutionDependencies { bridge, evm_signer },
        } = inputs;
        let genesis_hash = inner.spec.genesis_hash();
        Self {
            inner,
            genesis_hash,
            bridge,
            final_extra_data: block_extra_data.clone(),
            block_extra_data,
            accounted_parent_artifact_provider,
            validate_execution_summary,
            block_hash,
            block_state_root,
            parent_hash,
            current_block_validator_fees: U256::ZERO,
            system_tx_execution_gas: 0,
            evm_signer,
            expected_begin_system_txs,
            ethereum_post_execution_requests: None,
            system_layout_error,
            parent_consensus_metadata,
            proposer_evm_address,
            execute_outbe_block_hooks,
            // Placeholder. `apply_pre_execution_changes` computes the real
            // initial value once `block_number` is known and the Phase 1
            // preflight ran (or did not run).
            system_tx_phase_cursor: crate::system_tx::SystemTxPhase::UserTxs,
            prebuilt_phase1_tx,
            parent_artifact_hint,
            // `verify_phase1_in_preexec` populates this on a real
            // verify. It remains `None` for skip paths.
            verified_phase1_vrf_proof_hash: None,
            pending_tee_bootstrap,
            whitelisted_hook_event_logs: Vec::new(),
            zero_fee_soft_failures: 0,
            sponsored_gas_used: 0,
            witness_state: None,
            body_tx_index: 0,
            frozen_enclave_witness: None,
        }
    }
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub(crate) fn current_execution_summary(&self) -> ExecutionSummaryArtifact
    where
        Evm: reth_ethereum::evm::primitives::Evm,
    {
        // ExecutionSummaryArtifact wire format v0x04 carries
        // only `validator_fee_sum`. The per-block emission field is
        // removed. The Cycle handler computes daily emission from the
        // closed-form formula, so emission does not need to travel in
        // `extra_data`.
        ExecutionSummaryArtifact {
            validator_fee_sum: self.current_block_validator_fees,
        }
    }

    /// Canonical final header `extra_data` bytes used by `finish()` for
    /// execution-summary validation and bridge recording.
    pub(crate) fn final_extra_data(&self) -> &Bytes {
        &self.final_extra_data
    }

    pub(crate) fn set_final_extra_data(&mut self, bytes: Bytes) {
        self.final_extra_data = bytes;
    }

    /// Pre-encodes the final execution-produced header fields.
    /// The block-builder adapter owns the encoding. This executor entry point
    /// lets the opaque Reth builder path invoke it before parallel root freeze.
    pub fn prepare_final_header_artifacts(
        &mut self,
        timestamp_millis_part: u64,
    ) -> Result<(), BlockExecutionError>
    where
        Evm: reth_ethereum::evm::primitives::Evm,
    {
        let enclave_witness = self.final_enclave_witness_bytes()?;
        self.final_extra_data = crate::builder::encode_final_header_artifacts(
            self.final_extra_data.as_ref(),
            self.current_execution_summary(),
            timestamp_millis_part,
            enclave_witness,
        )?;
        Ok(())
    }

    // Half C-parlia step 11: `set_pending_consensus_metadata` and
    // `ingest_consensus_metadata_tx` are deleted. Finalized-parent
    // metadata now lives in the begin-zone Phase 1 system transaction input.
    // The pre-exec dispatch arm at `execute_transaction_with_commit_condition`
    // no longer accepts consensus metadata transactions. The proposer no
    // longer produces them.
}

#[allow(private_bounds)]
impl<DB, E> BlockExecutor for OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    // outbe-evm is pinned to revm's standard `HaltReason`; this constraint
    // is what lets [`system_tx_failure_code_for_result`] pattern-match the
    // halt variants for soft-failure code assignment.
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, reth_ethereum::TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.apply_outbe_pre_execution()
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        crate::executor::witness::clear_detached_trace_witness();
        outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
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
            self.inner.evm.set_witness_state(self.witness_state.clone());
        }
        let _witness_scope = self.prepare_tx_witness_scope(false)?;
        self.inner
            .execute_transaction_without_commit(WithTxEnv {
                tx: Arc::new(recovered),
                tx_env,
            })
            .map_err(preserve_cancelled_execution)
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&Self::Result) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        crate::executor::witness::clear_detached_trace_witness();
        outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        let (tx_env, recovered) = tx.into_parts();
        let tx_hash = *recovered.tx().tx_hash();
        let is_system_tx = is_reserved_system_tx(recovered.tx());
        if is_system_tx && self.is_preexecuted_phase1_witness(recovered.tx()) {
            let outcome = self.execute_reserved_system_tx(recovered, f);
            if outcome.is_ok() {
                self.advance_preexecuted_phase1_witness_index()?;
            }
            return outcome.map_err(preserve_cancelled_execution);
        }
        let is_user_candidate = !is_system_tx;
        let witness_scope = self.prepare_tx_witness_scope(is_user_candidate)?;
        let witness_state = self.witness_state.clone();
        let body_tx_index = self.body_tx_index;
        let base_extra_data = self.block_extra_data.clone();
        let wrapped_f = |result: &Self::Result| -> CommitChanges {
            if !Self::check_witness_commit_allowed_with(
                witness_state.as_ref(),
                body_tx_index,
                base_extra_data.as_ref(),
                is_user_candidate,
            ) {
                return CommitChanges::No;
            }
            let decision = f(result);
            if !decision.should_commit() {
                if let Some(state) = witness_state.as_ref() {
                    if let Ok(mut guard) = state.lock() {
                        guard.staged.clear();
                        guard.replay_cursor = 0;
                    }
                }
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

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        self.inner.commit_transaction(output)
    }

    fn execute_block(
        mut self,
        transactions: impl IntoIterator<Item = impl ExecutableTx<Self>>,
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
    where
        Self: Sized,
    {
        self.apply_pre_execution_changes()?;

        for tx in transactions {
            self.execute_transaction_with_commit_condition(tx, |_| CommitChanges::Yes)?;
        }

        self.apply_post_execution_changes()
    }

    fn finish(mut self) -> Result<(Self::Evm, BlockExecutionResult<Receipt>), BlockExecutionError> {
        crate::executor::witness::clear_detached_trace_witness();
        let block_timestamp =
            outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
                .map_err(crate::errors::ExecutorValidationError::from)?;
        let current_summary = self.current_execution_summary();
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let block_artifacts =
            artifacts::validate_finish_artifacts(&self, block_number, current_summary)?;

        self.apply_outbe_ethereum_post_execution()?;
        let requests = self
            .ethereum_post_execution_requests
            .take()
            .ok_or(crate::errors::ExecutorValidationError::MissingPostExecutionOutput)?;
        let gas_used = if self.inner.evm.cfg_env().enable_amsterdam_eip8037 {
            self.inner.max_block_gas_used()
        } else {
            self.inner.cumulative_tx_gas_used
        };
        let result = BlockExecutionResult {
            receipts: std::mem::take(&mut self.inner.receipts),
            requests,
            gas_used,
            blob_gas_used: self.inner.blob_gas_used,
        };
        let evm = self.inner.evm;
        // Validator/import execution ends before Reth validates receipt and
        // state roots, so it must not publish speculative execution artifacts here. The
        // proposer publishes only after block assembly supplies the final hash.
        // A finalized validator block is reconstructed from durable canonical
        // receipts after the DB-only persistence barrier.
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

        Ok((evm, result))
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }
}

/// The upstream EVM variant stores its boxed cause without exposing it as source.
/// Unwrap only a proven cancellation or node-local abort so the engine's internal-error path retains it.
fn preserve_cancelled_execution(error: BlockExecutionError) -> BlockExecutionError {
    match error {
        BlockExecutionError::Internal(InternalBlockExecutionError::EVM { hash, error }) => {
            if outbe_primitives::projection::ExecutionReadCancelled::find(error.as_ref()).is_some()
                || outbe_primitives::projection::NodeLocalExecutionAbort::find(error.as_ref())
                    .is_some()
            {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(error))
            } else {
                BlockExecutionError::Internal(InternalBlockExecutionError::EVM { hash, error })
            }
        }
        error => error,
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use outbe_primitives::projection::{
        ExecutionReadBudget, ExecutionReadCancelled, NodeLocalExecutionAbort,
    };
    use revm::context_interface::result::{AnyError, EVMError};

    #[test]
    fn raw_execution_abort_is_internal_and_keeps_its_typed_cause() {
        let budget = ExecutionReadBudget::new();
        budget.cancel();
        let evm_error: EVMError<std::convert::Infallible> =
            EVMError::CustomAny(AnyError::new(ExecutionReadCancelled {
                budget: budget.clone(),
            }));
        let error = BlockExecutionError::Internal(InternalBlockExecutionError::EVM {
            hash: B256::ZERO,
            error: Box::new(evm_error),
        });
        let error = preserve_cancelled_execution(error);
        assert!(matches!(error, BlockExecutionError::Internal(_)));
        let cancelled = ExecutionReadCancelled::find(&error).unwrap();
        assert!(cancelled.budget.same_request(&budget));
    }

    #[test]
    fn node_local_execution_abort_is_preserved_across_evm_error() {
        let evm_error: EVMError<std::convert::Infallible> = EVMError::CustomAny(AnyError::new(
            NodeLocalExecutionAbort::new("TEE sidecar down"),
        ));
        let error = BlockExecutionError::Internal(InternalBlockExecutionError::EVM {
            hash: B256::ZERO,
            error: Box::new(evm_error),
        });
        let error = preserve_cancelled_execution(error);
        assert!(matches!(error, BlockExecutionError::Internal(_)));
        let abort = NodeLocalExecutionAbort::find(&error).unwrap();
        assert_eq!(abort.reason, "TEE sidecar down");
    }
}
